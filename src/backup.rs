use crate::claude::*;
use crate::list::backup_root;
use crate::manifest::{MANIFEST_FORMAT, MANIFEST_NAME, Manifest, SessionEntry};
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local, SecondsFormat};
use flate2::Compression;
use flate2::write::GzEncoder;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

#[derive(clap::Args, Debug, Default)]
pub struct BackupArgs {
    /// Project folder (default: current directory)
    #[arg(long)]
    pub project: Option<PathBuf>,
    /// Backup root (default: ~/claude-backups, or $CLAUDE_BACKUP_DIR)
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Comma-separated session id prefixes to include (default: all)
    #[arg(long)]
    pub sessions: Option<String>,
    /// Only sessions modified in the last DAYS days
    #[arg(long, value_name = "DAYS")]
    pub since: Option<f64>,
    /// Skip /rewind checkpoints (smaller archive)
    #[arg(long)]
    pub no_file_history: bool,
    /// After writing, delete all but the newest N archives of this project
    #[arg(long, value_name = "N")]
    pub keep: Option<usize>,
    #[arg(long)]
    pub dry_run: bool,
    /// Store the archive in <project>/.claude-backup/ instead of the backup root. Once that
    /// folder exists, later runs without --out keep using it.
    #[arg(long, conflicts_with = "out")]
    pub here: bool,
}

/// Make git ignore `.claude-backup/` through `.git/info/exclude`, which touches no tracked
/// file. Returns a line to print when something was added; `None` outside a git repository.
fn ensure_git_excluded(project: &Path) -> Option<String> {
    let exclude = git_exclude_path(project)?;
    let pattern = format!("{LOCAL_DIR}/");
    let existing = fs::read_to_string(&exclude).unwrap_or_default();
    let mut note = None;
    if !existing.lines().any(|l| l.trim() == pattern) {
        if let Some(p) = exclude.parent() {
            let _ = fs::create_dir_all(p);
        }
        if let Ok(mut f) = OpenOptions::new().append(true).create(true).open(&exclude) {
            let sep = if existing.is_empty() || existing.ends_with('\n') {
                ""
            } else {
                "\n"
            };
            use std::io::Write;
            let _ = writeln!(f, "{sep}{pattern}");
            note = Some(format!(
                "git ignore   added {pattern} to {}",
                exclude.display()
            ));
        }
    }
    if !git_ignores(project, LOCAL_DIR) {
        warn(format!(
            "{LOCAL_DIR}/ is not ignored by git (a .gitignore rule may override it); transcripts must never be committed"
        ));
    }
    note
}

pub fn default_out() -> PathBuf {
    match std::env::var("CLAUDE_BACKUP_DIR") {
        Ok(v) if !v.is_empty() => expand_tilde(&v),
        _ => home().join("claude-backups"),
    }
}

pub fn parse_prefixes(s: &Option<String>) -> Option<BTreeSet<String>> {
    s.as_ref().map(|s| {
        s.split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect()
    })
}

pub fn resolve_project(p: &Option<PathBuf>) -> Result<PathBuf> {
    let p = match p {
        Some(p) => expand_tilde(&path_str(p)),
        None => std::env::current_dir()?,
    };
    let p = if p.is_absolute() {
        p
    } else {
        std::env::current_dir()?.join(p)
    };
    Ok(fs::canonicalize(&p).unwrap_or(p))
}

type Tar = tar::Builder<GzEncoder<File>>;

fn add_file(tar: &mut Tar, path: &Path, arcname: &str) -> Result<()> {
    tar.append_path_with_name(path, arcname)
        .with_context(|| format!("adding {}", path.display()))
}

fn add_tree(tar: &mut Tar, root: &Path, arcname: &str) -> Result<()> {
    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .sort_by_file_name()
    {
        let entry = entry?;
        if !wanted_in_archive(&entry) {
            continue;
        }
        let rel = entry.path().strip_prefix(root)?;
        let name = if rel.as_os_str().is_empty() {
            arcname.to_string()
        } else {
            format!("{arcname}/{}", path_str(rel))
        };
        if entry.file_type().is_dir() {
            tar.append_dir(&name, entry.path())?;
        } else {
            add_file(tar, entry.path(), &name)?;
        }
    }
    Ok(())
}

fn add_bytes(tar: &mut Tar, name: &str, data: &[u8]) -> Result<()> {
    let mut h = tar::Header::new_gnu();
    h.set_size(data.len() as u64);
    h.set_mode(0o600);
    h.set_mtime(
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_secs(),
    );
    tar.append_data(&mut h, name, data)?;
    Ok(())
}

pub fn run(args: &BackupArgs) -> Result<()> {
    let cfg = config_dir();
    let project = resolve_project(&args.project)?;
    if !project.exists() {
        warn(format!(
            "folder {} does not exist; backing up whatever Claude Code still holds for it",
            project.display()
        ));
    }
    let Some(project_dir) = find_project_dir(&cfg, &project) else {
        bail!(
            "no Claude Code data found for {} under {}/projects",
            project.display(),
            cfg.display()
        );
    };

    let root = git_root(&project).unwrap_or_else(|| project.clone());
    let mem_dir = memory_dir(&cfg, &root);
    let pl_dir = plans_dir(&cfg, &root);
    let tasks_root = cfg.join("tasks");
    let task_ids: Vec<String> = fs::read_dir(&tasks_root)
        .map(|rd| {
            let mut v: Vec<String> = rd
                .flatten()
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            v.sort();
            v
        })
        .unwrap_or_default();

    let wanted = parse_prefixes(&args.sessions);
    let cutoff = args
        .since
        .map(|d| SystemTime::now() - Duration::from_secs_f64(d * 86400.0));

    let mut sessions: Vec<SessionEntry> = Vec::new();
    let mut plan_files: BTreeMap<String, PathBuf> = BTreeMap::new();
    let mut task_dirs: BTreeMap<String, PathBuf> = BTreeMap::new();
    let canon_plans = fs::canonicalize(&pl_dir).unwrap_or(pl_dir.clone());

    for t in transcripts_in(&project_dir) {
        let file = t.file_name().unwrap().to_string_lossy().into_owned();
        let Some(sid) = session_id_of(&file) else {
            continue;
        };
        if let Some(w) = &wanted
            && !w.iter().any(|p| sid.starts_with(p))
        {
            continue;
        }
        let modified = mtime(&t);
        if cutoff.is_some_and(|c| modified < c) {
            continue;
        }
        let info = scan_transcript(&t, &task_ids);
        let sdir = project_dir.join(&sid);
        let mut entry = SessionEntry {
            id: sid.clone(),
            file,
            size: fs::metadata(&t)?.len(),
            modified: DateTime::<Local>::from(modified)
                .format("%Y-%m-%dT%H:%M:%S")
                .to_string(),
            title: if sdir.is_dir() {
                custom_title(&sdir)
            } else {
                String::new()
            },
            first_prompt: info.first_prompt,
            claude_version: info.version,
            plans: Vec::new(),
            tasks: info.tasks.clone(),
            file_history: !args.no_file_history && cfg.join("file-history").join(&sid).is_dir(),
        };
        for slug in &info.slugs {
            let p = pl_dir.join(format!("{slug}.md"));
            if p.is_file() {
                let name = format!("{slug}.md");
                plan_files.insert(name.clone(), p);
                entry.plans.push(name);
            }
        }
        for raw in &info.plan_paths {
            let p = PathBuf::from(raw);
            let parent_ok = p
                .parent()
                .and_then(|d| fs::canonicalize(d).ok())
                .is_some_and(|d| d == canon_plans);
            if p.is_file() && parent_ok {
                let name = p.file_name().unwrap().to_string_lossy().into_owned();
                if !entry.plans.contains(&name) {
                    entry.plans.push(name.clone());
                }
                plan_files.insert(name, p);
            }
        }
        for tid in &info.tasks {
            task_dirs.insert(tid.clone(), tasks_root.join(tid));
        }
        sessions.push(entry);
    }
    if sessions.is_empty() {
        bail!("no sessions matched in {}", project_dir.display());
    }

    // prompt history lines for this project
    let mut history_lines: Vec<String> = Vec::new();
    if let Ok(f) = File::open(cfg.join("history.jsonl")) {
        for line in BufReader::new(f).split(b'\n').flatten() {
            let line = String::from_utf8_lossy(&line).into_owned();
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(rec) = serde_json::from_str::<Value>(&line)
                && rec.get("project").and_then(Value::as_str) == Some(path_str(&project).as_str())
            {
                history_lines.push(line);
            }
        }
    }

    // project entry from ~/.claude.json
    let claude_json_entry: Option<Value> = fs::read(claude_json_path())
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v.get("projects")?.get(path_str(&project)).cloned());

    let manifest = Manifest {
        format: MANIFEST_FORMAT,
        tool_version: env!("CARGO_PKG_VERSION").to_string(),
        created_at: Local::now().to_rfc3339_opts(SecondsFormat::Secs, false),
        hostname: gethostname::gethostname().to_string_lossy().into_owned(),
        config_dir: path_str(&cfg),
        project_path: path_str(&project),
        project_encoded: project_dir
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        memory_root: path_str(&root),
        memory_included: mem_dir.is_dir(),
        plans: plan_files.keys().cloned().collect(),
        tasks: task_dirs.keys().cloned().collect(),
        history_lines: history_lines.len(),
        claude_json_entry_included: claude_json_entry.is_some(),
        sessions: sessions.clone(),
    };

    let local_dir = local_backup_dir(&project);
    let use_local = args.here || (args.out.is_none() && local_dir.is_dir());
    if args.here && !project.is_dir() {
        bail!("--here needs the folder to exist: {}", project.display());
    }
    let out_dir = if use_local {
        local_dir
    } else {
        backup_root(&args.out).join(&manifest.project_encoded)
    };
    let base = project
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "root".into());
    let archive = out_dir.join(format!(
        "{base}-{}.tar.gz",
        Local::now().format("%Y%m%d-%H%M%S")
    ));

    log(format!("project      {}", project.display()));
    log(format!("claude data  {}", project_dir.display()));
    log(format!(
        "sessions     {}{}",
        sessions.len(),
        if args.no_file_history {
            "  (file-history skipped)"
        } else {
            ""
        }
    ));
    log(format!("plans        {}", plan_files.len()));
    log(format!(
        "memory       {}",
        if mem_dir.is_dir() {
            path_str(&mem_dir)
        } else {
            "none".into()
        }
    ));
    log(format!("task lists   {}", task_dirs.len()));
    log(format!("history      {} prompt lines", history_lines.len()));
    log(format!("archive      {}", archive.display()));
    if args.dry_run {
        for s in &sessions {
            log(format!(
                "  {}  {:<8} {}",
                &s.id[..8],
                human(s.size),
                s.label()
            ));
        }
        log("dry run: nothing written");
        return Ok(());
    }

    fs::create_dir_all(&out_dir)?;
    if use_local && let Some(note) = ensure_git_excluded(&project) {
        log(note);
    }
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&archive)
        .with_context(|| format!("creating {}", archive.display()))?;
    let mut tar = tar::Builder::new(GzEncoder::new(file, Compression::new(6)));

    add_bytes(
        &mut tar,
        MANIFEST_NAME,
        &serde_json::to_vec_pretty(&manifest)?,
    )?;
    for s in &sessions {
        add_file(
            &mut tar,
            &project_dir.join(&s.file),
            &format!("project/{}", s.file),
        )?;
        let sdir = project_dir.join(&s.id);
        if sdir.is_dir() {
            add_tree(&mut tar, &sdir, &format!("project/{}", s.id))?;
        }
        if s.file_history {
            add_tree(
                &mut tar,
                &cfg.join("file-history").join(&s.id),
                &format!("file-history/{}", s.id),
            )?;
        }
        let env = cfg.join("session-env").join(&s.id);
        if env.is_dir() {
            add_tree(&mut tar, &env, &format!("session-env/{}", s.id))?;
        }
    }
    // loose project-level files (e.g. .session-aliases), but not other sessions' data
    for e in fs::read_dir(&project_dir)?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if e.path().is_file() && session_id_of(&name).is_none() {
            add_file(&mut tar, &e.path(), &format!("project/{name}"))?;
        }
    }
    if mem_dir.is_dir() {
        add_tree(&mut tar, &mem_dir, "memory")?;
    }
    for (name, p) in &plan_files {
        add_file(&mut tar, p, &format!("plans/{name}"))?;
    }
    for (tid, p) in &task_dirs {
        if p.is_dir() {
            add_tree(&mut tar, p, &format!("tasks/{tid}"))?;
        }
    }
    if !history_lines.is_empty() {
        let data = history_lines.join("\n") + "\n";
        add_bytes(&mut tar, "history.jsonl", data.as_bytes())?;
    }
    if let Some(entry) = &claude_json_entry {
        add_bytes(
            &mut tar,
            "claude-json-project.json",
            &serde_json::to_vec_pretty(entry)?,
        )?;
    }
    tar.into_inner()?.finish()?;

    log(format!(
        "wrote        {} ({})",
        archive.display(),
        human(fs::metadata(&archive)?.len())
    ));
    log(
        "note: transcripts are plaintext and can contain file contents and secrets; keep this archive private",
    );

    if let Some(keep) = args.keep {
        let mut old: Vec<PathBuf> = fs::read_dir(&out_dir)?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file() && path_str(p).ends_with(".tar.gz"))
            .collect();
        old.sort_by_key(|p| std::cmp::Reverse(mtime(p)));
        for p in old.into_iter().skip(keep) {
            fs::remove_file(&p)?;
            log(format!("pruned       {}", p.display()));
        }
    }
    Ok(())
}
