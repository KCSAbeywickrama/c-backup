use crate::backup::{parse_prefixes, resolve_project};
use crate::claude::*;
use crate::list::{
    all_archives, archives_in, backup_root, pick_archive, print_archives, resolve_archive_arg,
};
use crate::manifest::{MANIFEST_NAME, open_archive, read_manifest};
use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions, Permissions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

#[derive(clap::Args, Debug)]
pub struct RestoreArgs {
    /// Archive to restore (default: the newest backup of this folder, or a pick from all backups)
    pub archive: Option<PathBuf>,
    /// Backup root to look in when no archive is given (default: ~/claude-backups, or $CLAUDE_BACKUP_DIR)
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Restore for this folder (default: current directory)
    #[arg(long)]
    pub project: Option<PathBuf>,
    /// Restore to the path recorded in the archive
    #[arg(long)]
    pub original: bool,
    /// Comma-separated session id prefixes to restore (default: all)
    #[arg(long)]
    pub sessions: Option<String>,
    /// Overwrite files that already exist
    #[arg(long)]
    pub force: bool,
    /// Also restore the ~/.claude.json project entry (trust, allowed tools, MCP)
    #[arg(long)]
    pub with_config: bool,
    #[arg(long)]
    pub dry_run: bool,
    /// Print every file
    #[arg(short, long)]
    pub verbose: bool,
    /// When the folder differs from the archive's, rewrite the old path without asking (non-interactive use)
    #[arg(long, conflicts_with = "no_rewrite_paths")]
    pub rewrite_paths: bool,
    /// When the folder differs from the archive's, keep recorded paths as they are without asking (non-interactive use)
    #[arg(long)]
    pub no_rewrite_paths: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Rewrite {
    NotNeeded,
    Yes,
    Declined,
    Skipped,
}

fn is_path_boundary(b: Option<&u8>) -> bool {
    match b {
        None => true,
        Some(&c) => !(c.is_ascii_alphanumeric() || c == b'-' || c == b'_' || c == b'.'),
    }
}

/// The byte before a match must not belong to a name. Inside JSON strings a line break or tab
/// is the two bytes `\n` / `\t`, so a path at the start of a line is preceded by that letter.
fn boundary_before(data: &[u8], pos: usize) -> bool {
    if pos == 0 {
        return true;
    }
    let prev = data[pos - 1];
    if is_path_boundary(Some(&prev)) {
        return true;
    }
    matches!(prev, b'n' | b't' | b'r') && pos >= 2 && data[pos - 2] == b'\\'
}

/// Replace `old` with `new` wherever `old` stands as a whole path or path prefix: not glued to
/// a name character on either side. `/w/A` matches in `"/w/A"` and `/w/A/x.rs` but not in
/// `/w/A-other`, `/w/A.bak` or `/x/w/A`. Returns the rewritten bytes and the replacement count.
pub fn rewrite_paths(data: &[u8], old: &[u8], new: &[u8]) -> (Vec<u8>, usize) {
    if old.is_empty() || old == new {
        return (data.to_vec(), 0);
    }
    let mut out = Vec::with_capacity(data.len());
    let mut last = 0;
    let mut n = 0;
    for pos in memchr::memmem::find_iter(data, old) {
        if !boundary_before(data, pos) || !is_path_boundary(data.get(pos + old.len())) {
            continue;
        }
        out.extend_from_slice(&data[last..pos]);
        out.extend_from_slice(new);
        last = pos + old.len();
        n += 1;
    }
    out.extend_from_slice(&data[last..]);
    (out, n)
}

fn rewritable(name: &Path) -> bool {
    name.iter()
        .next()
        .is_some_and(|h| h == "project" || h == "memory" || h == "plans")
}

struct Targets<'a> {
    cfg: &'a Path,
    project: PathBuf,
    memory: PathBuf,
    plans: PathBuf,
    keep_ids: &'a BTreeSet<String>,
}

impl Targets<'_> {
    fn dest_for(&self, name: &Path) -> Option<PathBuf> {
        let mut it = name.iter();
        let head = it.next()?.to_string_lossy().into_owned();
        let rest: PathBuf = it.collect();
        let rest_s = path_str(&rest);
        let first = rest
            .iter()
            .next()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let session_filtered =
            |first: &str| session_id_of(first).is_some_and(|sid| !self.keep_ids.contains(&sid));
        match head.as_str() {
            "project" => {
                if session_filtered(&first) {
                    return None;
                }
                Some(self.project.join(&rest))
            }
            "memory" => Some(if rest_s.is_empty() {
                self.memory.clone()
            } else {
                self.memory.join(&rest)
            }),
            "plans" => Some(self.plans.join(&rest)),
            "file-history" | "session-env" => {
                if session_filtered(&first) {
                    return None;
                }
                Some(self.cfg.join(&head).join(&rest))
            }
            "tasks" => Some(self.cfg.join("tasks").join(&rest)),
            _ => None,
        }
    }
}

fn hist_key(rec: &Value) -> (String, String, String, String) {
    let g = |k: &str| rec.get(k).map(Value::to_string).unwrap_or_default();
    (g("project"), g("sessionId"), g("timestamp"), g("display"))
}

fn hist_sid(line: &str) -> Option<String> {
    serde_json::from_str::<Value>(line)
        .ok()?
        .get("sessionId")?
        .as_str()
        .map(str::to_string)
}

fn merge_history(cfg: &Path, new_lines: &[String], dry_run: bool) -> Result<usize> {
    let hist = cfg.join("history.jsonl");
    let mut seen = BTreeSet::new();
    let mut needs_newline = false;
    if let Ok(raw) = fs::read(&hist) {
        needs_newline = !raw.is_empty() && !raw.ends_with(b"\n");
        for line in String::from_utf8_lossy(&raw).lines() {
            if let Ok(rec) = serde_json::from_str::<Value>(line) {
                seen.insert(hist_key(&rec));
            }
        }
    }
    let mut to_add = Vec::new();
    for line in new_lines {
        let Ok(rec) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if seen.insert(hist_key(&rec)) {
            to_add.push(line.as_str());
        }
    }
    if !to_add.is_empty() && !dry_run {
        fs::create_dir_all(cfg)?;
        let mut f = OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(&hist)?;
        if needs_newline {
            f.write_all(b"\n")?;
        }
        f.write_all((to_add.join("\n") + "\n").as_bytes())?;
    }
    Ok(to_add.len())
}

fn merge_claude_json(entry: &Value, project: &Path, force: bool, dry_run: bool) -> Result<String> {
    let cj = claude_json_path();
    let mut data: Value = match fs::read(&cj) {
        Ok(b) => serde_json::from_slice(&b).context("~/.claude.json is not valid JSON")?,
        Err(_) => Value::Object(Map::new()),
    };
    let obj = data
        .as_object_mut()
        .context("~/.claude.json is not a JSON object")?;
    let projects = obj
        .entry("projects")
        .or_insert_with(|| Value::Object(Map::new()));
    let projects = projects
        .as_object_mut()
        .context("\"projects\" in ~/.claude.json is not an object")?;
    let key = path_str(project);
    if projects.contains_key(&key) && !force {
        return Ok("kept existing entry (use --force to replace)".into());
    }
    if dry_run {
        return Ok("would write entry".into());
    }
    projects.insert(key, entry.clone());
    if cj.is_file() {
        let bdir = config_dir().join("backups");
        fs::create_dir_all(&bdir)?;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)?
            .as_millis();
        fs::copy(
            &cj,
            bdir.join(format!(".claude.json.claude-backup.{stamp}")),
        )?;
    }
    let parent = cj.parent().context("bad ~/.claude.json path")?;
    fs::create_dir_all(parent)?;
    let mut tmp = tempfile::Builder::new()
        .prefix(".claude.json.")
        .tempfile_in(parent)?;
    tmp.write_all(&serde_json::to_vec_pretty(&data)?)?;
    fs::set_permissions(tmp.path(), Permissions::from_mode(0o600))?;
    tmp.persist(&cj)?;
    Ok("wrote entry".into())
}

/// No archive given: look in the folder's own `.claude-backup/` first, then its backups under
/// the backup root. One candidate is used as is; several are offered for a pick (without a
/// terminal the newest local one, else the newest under the root, is used). When the folder has
/// none at all, typically right after a rename, every backup is listed for a pick.
fn choose_archive(args: &RestoreArgs) -> Result<Option<PathBuf>> {
    let project = resolve_project(&args.project)?;
    let root = backup_root(&args.out);
    let local_dir = local_backup_dir(&project);
    let own_dir = root.join(encode_path(&path_str(&project)));
    let mut candidates = archives_in(&local_dir);
    let n_local = candidates.len();
    candidates.extend(archives_in(&own_dir));
    if !candidates.is_empty() {
        if candidates.len() > 1 && can_ask() {
            if n_local > 0 {
                log(format!("backups in {}", local_dir.display()));
                print_archives(&candidates[..n_local], false, 1);
            }
            if candidates.len() > n_local {
                log(format!(
                    "backups for {} in {}",
                    project.display(),
                    own_dir.display()
                ));
                print_archives(&candidates[n_local..], false, n_local + 1);
            }
            return Ok(pick_archive(&candidates).map(|a| a.path.clone()));
        }
        return Ok(Some(candidates[0].path.clone()));
    }
    let all = all_archives(&root);
    if all.is_empty() {
        bail!(
            "no backups under {} or in {}",
            root.display(),
            local_dir.display()
        );
    }
    log(format!(
        "no backups for {}; all backups under {}:",
        project.display(),
        root.display()
    ));
    print_archives(&all, true, 1);
    if !can_ask() {
        bail!("no terminal to pick from; pass the archive path");
    }
    Ok(pick_archive(&all).map(|a| a.path.clone()))
}

pub fn run(args: &RestoreArgs) -> Result<()> {
    let archive = match &args.archive {
        Some(a) => {
            resolve_archive_arg(a, &resolve_project(&args.project)?, &backup_root(&args.out))?
        }
        None => match choose_archive(args)? {
            Some(a) => a,
            None => {
                log("cancelled");
                return Ok(());
            }
        },
    };
    let manifest = read_manifest(&archive)?;
    let cfg = config_dir();

    let project = if args.original {
        PathBuf::from(&manifest.project_path)
    } else {
        resolve_project(&args.project)?
    };
    let moved = path_str(&project) != manifest.project_path;
    let root = git_root(&project).unwrap_or_else(|| project.clone());
    let dest_project = cfg.join("projects").join(encode_path(&path_str(&project)));
    let dest_memory = memory_dir(&cfg, &root);
    let dest_plans = plans_dir(&cfg, &root);

    let wanted = parse_prefixes(&args.sessions);
    let keep_ids: BTreeSet<String> = manifest
        .sessions
        .iter()
        .filter(|s| {
            wanted
                .as_ref()
                .is_none_or(|w| w.iter().any(|p| s.id.starts_with(p)))
        })
        .map(|s| s.id.clone())
        .collect();
    if wanted.is_some() && keep_ids.is_empty() {
        bail!("no sessions in the archive match --sessions");
    }

    log(format!("archive      {}", archive.display()));
    log(format!(
        "backed up    {} on {} ({})",
        manifest.project_path, manifest.hostname, manifest.created_at
    ));
    log(format!(
        "restore to   {}{}",
        project.display(),
        if moved { "   [different path]" } else { "" }
    ));
    log(format!("claude data  {}", dest_project.display()));
    log(format!("memory       {}", dest_memory.display()));
    log(format!("plans        {}", dest_plans.display()));
    log(format!(
        "sessions     {} of {}",
        keep_ids.len(),
        manifest.sessions.len()
    ));
    if args.dry_run {
        log("mode         dry run");
    }

    let rewrite = if !moved {
        Rewrite::NotNeeded
    } else if args.rewrite_paths {
        Rewrite::Yes
    } else if args.no_rewrite_paths {
        Rewrite::Declined
    } else if can_ask() {
        log("");
        log(format!(
            "This archive was made for  {}",
            manifest.project_path
        ));
        log(format!("You are restoring it to    {}", project.display()));
        let yes = confirm(
            "Rewrite that path inside the restored transcripts, memory and plans so that\nresumed sessions and /rewind refer to the new folder?",
            true,
        );
        log("");
        if yes { Rewrite::Yes } else { Rewrite::Declined }
    } else {
        Rewrite::Skipped
    };
    let old_path = manifest.project_path.as_bytes();
    let new_path = path_str(&project);
    let mut rewritten_refs = 0usize;
    let mut rewritten_files = 0usize;

    let live = running_sessions_for(&cfg, &project);
    if !live.is_empty() && args.force && !args.dry_run {
        let pids: Vec<String> = live.iter().map(u64::to_string).collect();
        bail!(
            "Claude Code is running on this project (pid {}); close it before restoring with --force",
            pids.join(", ")
        );
    }

    let targets = Targets {
        cfg: &cfg,
        project: dest_project,
        memory: dest_memory,
        plans: dest_plans,
        keep_ids: &keep_ids,
    };
    let mut written = 0usize;
    let mut skipped = 0usize;
    let mut history_lines: Vec<String> = Vec::new();
    let mut claude_entry: Option<Value> = None;

    let mut tar = open_archive(&archive)?;
    for entry in tar.entries()? {
        let mut entry = entry?;
        let name = entry.path()?.into_owned();
        if name.as_os_str() == MANIFEST_NAME {
            continue;
        }
        if !safe_member(&name) {
            warn(format!("skipping unsafe member name {:?}", name));
            continue;
        }
        if name.as_os_str() == "history.jsonl" {
            let mut s = String::new();
            entry.read_to_string(&mut s)?;
            history_lines = s.lines().map(str::to_string).collect();
            continue;
        }
        if name.as_os_str() == "claude-json-project.json" {
            let mut s = String::new();
            entry.read_to_string(&mut s)?;
            claude_entry = serde_json::from_str(&s).ok();
            continue;
        }
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let Some(dest) = targets.dest_for(&name) else {
            continue;
        };
        if dest.exists() && !args.force {
            skipped += 1;
            if args.verbose {
                log(format!("  keep   {}", dest.display()));
            }
            continue;
        }
        let rewrite_this = rewrite == Rewrite::Yes && rewritable(&name);
        if args.dry_run {
            written += 1;
            if rewrite_this {
                let mut buf = Vec::new();
                entry.read_to_end(&mut buf)?;
                let (_, n) = rewrite_paths(&buf, old_path, new_path.as_bytes());
                if n > 0 {
                    rewritten_refs += n;
                    rewritten_files += 1;
                }
            }
            if args.verbose {
                log(format!("  write  {}", dest.display()));
            }
            continue;
        }
        let parent = dest.parent().context("bad destination")?;
        fs::create_dir_all(parent)?;
        let mut tmp = tempfile::Builder::new()
            .prefix(".claude-backup.")
            .tempfile_in(parent)?;
        if rewrite_this {
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf)?;
            let (out, n) = rewrite_paths(&buf, old_path, new_path.as_bytes());
            tmp.write_all(&out)?;
            if n > 0 {
                rewritten_refs += n;
                rewritten_files += 1;
            }
        } else {
            std::io::copy(&mut entry, &mut tmp)?;
        }
        let mode = entry.header().mode().unwrap_or(0) & 0o777;
        fs::set_permissions(
            tmp.path(),
            Permissions::from_mode(if mode == 0 { 0o600 } else { mode }),
        )?;
        tmp.persist(&dest)
            .with_context(|| format!("writing {}", dest.display()))?;
        written += 1;
        if args.verbose {
            log(format!("  write  {}", dest.display()));
        }
    }

    let mut added = 0usize;
    if !history_lines.is_empty() {
        let lines: Vec<String> = if moved && !args.original {
            history_lines
                .iter()
                .filter_map(|l| serde_json::from_str::<Value>(l).ok())
                .filter(|rec| {
                    rec.get("sessionId")
                        .and_then(Value::as_str)
                        .is_none_or(|sid| keep_ids.contains(sid))
                })
                .map(|mut rec| {
                    rec["project"] = Value::String(path_str(&project));
                    rec.to_string()
                })
                .collect()
        } else {
            history_lines
                .into_iter()
                .filter(|l| hist_sid(l).is_none_or(|sid| keep_ids.contains(&sid)))
                .collect()
        };
        added = merge_history(&cfg, &lines, args.dry_run)?;
    }

    let cfg_note = match (&claude_entry, args.with_config) {
        (None, _) => "no ~/.claude.json entry in the archive".to_string(),
        (Some(_), false) => "not restored (pass --with-config)".to_string(),
        (Some(entry), true) => merge_claude_json(entry, &project, args.force, args.dry_run)?,
    };

    log(format!(
        "files        {} {}, {} kept as they were",
        written,
        if args.dry_run {
            "would be written"
        } else {
            "written"
        },
        skipped
    ));
    log(format!(
        "history      {} prompt lines {}",
        added,
        if args.dry_run {
            "would be added"
        } else {
            "added"
        }
    ));
    log(format!("config       {cfg_note}"));
    match rewrite {
        Rewrite::NotNeeded => {}
        Rewrite::Yes => log(format!(
            "rewrite      {rewritten_refs} path references in {rewritten_files} files{}",
            if args.dry_run { " (dry run)" } else { "" }
        )),
        Rewrite::Declined => {
            log("rewrite      declined; recorded paths still point at the original folder")
        }
        Rewrite::Skipped => log(
            "rewrite      skipped: no terminal to ask (use --rewrite-paths or --no-rewrite-paths)",
        ),
    }
    if moved && rewrite != Rewrite::Yes {
        log(
            "note: /rewind checkpoints and recorded cwd inside transcripts point at the original folder",
        );
    }
    if !args.dry_run && written > 0 {
        log(format!(
            "resume with  cd {} && claude --resume",
            project.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::rewrite_paths;

    fn rw(data: &str) -> (String, usize) {
        let (out, n) = rewrite_paths(data.as_bytes(), b"/w/A", b"/w/B");
        (String::from_utf8(out).unwrap(), n)
    }

    #[test]
    fn rewrites_whole_path_and_prefix() {
        assert_eq!(rw(r#"{"cwd":"/w/A"}"#), (r#"{"cwd":"/w/B"}"#.into(), 1));
        assert_eq!(rw("/w/A/src/x.rs"), ("/w/B/src/x.rs".into(), 1));
        assert_eq!(rw("/w/A"), ("/w/B".into(), 1));
        assert_eq!(rw("a /w/A b /w/A/c"), ("a /w/B b /w/B/c".into(), 2));
    }

    #[test]
    fn leaves_other_paths_alone() {
        assert_eq!(rw("/w/A-other/x"), ("/w/A-other/x".into(), 0));
        assert_eq!(rw("/w/A.bak"), ("/w/A.bak".into(), 0));
        assert_eq!(rw("/w/A_1"), ("/w/A_1".into(), 0));
        assert_eq!(rw("/w/AB"), ("/w/AB".into(), 0));
        assert_eq!(rw("/x/w/A/y"), ("/x/w/A/y".into(), 0));
        assert_eq!(rw("no paths here"), ("no paths here".into(), 0));
    }

    #[test]
    fn json_escaped_newline_or_tab_before_path_is_a_boundary() {
        assert_eq!(rw(r"out\n/w/A/x"), (r"out\n/w/B/x".into(), 1));
        assert_eq!(rw(r"a\t/w/A"), (r"a\t/w/B".into(), 1));
        assert_eq!(rw("n/w/A"), ("n/w/A".into(), 0));
        assert_eq!(rw("\n/w/A"), ("\n/w/B".into(), 1));
    }

    #[test]
    fn old_path_prefix_of_new_is_single_pass() {
        let (out, n) = rewrite_paths(b"/w/A/f", b"/w/A", b"/w/A/sub");
        assert_eq!((out.as_slice(), n), (&b"/w/A/sub/f"[..], 1));
        let (out, n) = rewrite_paths(b"x", b"", b"/w/B");
        assert_eq!((out.as_slice(), n), (&b"x"[..], 0));
    }
}
