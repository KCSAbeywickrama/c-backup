use crate::backup::{default_out, resolve_project};
use crate::claude::*;
use crate::manifest::{Manifest, read_manifest};
use anyhow::{Result, bail};
use chrono::{DateTime, Local};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[derive(clap::Args, Debug)]
pub struct ListArgs {
    /// Archive to inspect
    pub archive: Option<PathBuf>,
    /// Project folder (default: current directory)
    #[arg(long)]
    pub project: Option<PathBuf>,
    /// Backup root (default: ~/claude-backups, or $CLAUDE_BACKUP_DIR)
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// List sessions currently in ~/.claude for this folder
    #[arg(long)]
    pub local: bool,
    /// List every backup under the backup root, not only this folder's
    #[arg(long)]
    pub all: bool,
}

pub struct ArchiveInfo {
    pub path: PathBuf,
    pub size: u64,
    pub manifest: Manifest,
}

pub fn backup_root(out: &Option<PathBuf>) -> PathBuf {
    out.clone()
        .map(|p| expand_tilde(&path_str(&p)))
        .unwrap_or_else(default_out)
}

/// Readable archives in one directory, newest first.
pub fn archives_in(dir: &Path) -> Vec<ArchiveInfo> {
    let mut v = Vec::new();
    let Ok(rd) = fs::read_dir(dir) else { return v };
    for p in rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && path_str(p).ends_with(".tar.gz"))
    {
        match read_manifest(&p) {
            Ok(manifest) => v.push(ArchiveInfo {
                size: fs::metadata(&p).map(|m| m.len()).unwrap_or(0),
                path: p,
                manifest,
            }),
            Err(e) => warn(format!("skipping {}: {e:#}", p.display())),
        }
    }
    v.sort_by(|a, b| b.manifest.created_at.cmp(&a.manifest.created_at));
    v
}

/// Every readable archive under the backup root, grouped by project. Projects with the newest
/// backup come first; within a project, newest first.
pub fn all_archives(root: &Path) -> Vec<ArchiveInfo> {
    let mut v: Vec<ArchiveInfo> = fs::read_dir(root)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .flat_map(|d| archives_in(&d))
                .collect()
        })
        .unwrap_or_default();
    let mut newest: BTreeMap<String, String> = BTreeMap::new();
    for a in &v {
        let e = newest.entry(a.manifest.project_path.clone()).or_default();
        if a.manifest.created_at > *e {
            *e = a.manifest.created_at.clone();
        }
    }
    v.sort_by(|a, b| {
        newest[&b.manifest.project_path]
            .cmp(&newest[&a.manifest.project_path])
            .then_with(|| a.manifest.project_path.cmp(&b.manifest.project_path))
            .then_with(|| b.manifest.created_at.cmp(&a.manifest.created_at))
    });
    v
}

/// Print archives numbered from 1; with `grouped`, a heading per project.
pub fn print_archives(items: &[ArchiveInfo], grouped: bool) {
    let mut last = "";
    for (i, a) in items.iter().enumerate() {
        if grouped && a.manifest.project_path != last {
            log(format!("  {}", a.manifest.project_path));
            last = &a.manifest.project_path;
        }
        log(format!(
            "  {:>3}  {}  {:>9}  {:>3} sessions  {}",
            i + 1,
            a.manifest
                .created_at
                .chars()
                .take(19)
                .collect::<String>()
                .replace('T', " "),
            human(a.size),
            a.manifest.sessions.len(),
            a.path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        ));
    }
}

/// Ask which archive to use. `None` when cancelled with Enter, `q` or EOF.
pub fn pick_archive(items: &[ArchiveInfo]) -> Option<&ArchiveInfo> {
    loop {
        print!("restore which? [1-{}, Enter cancels] ", items.len());
        let _ = io::stdout().flush();
        let mut line = String::new();
        if io::stdin().read_line(&mut line).unwrap_or(0) == 0 {
            return None;
        }
        let t = line.trim();
        if t.is_empty() || t.eq_ignore_ascii_case("q") {
            return None;
        }
        match t.parse::<usize>() {
            Ok(n) if (1..=items.len()).contains(&n) => return Some(&items[n - 1]),
            _ => log(format!(
                "please enter a number between 1 and {}",
                items.len()
            )),
        }
    }
}

pub fn run(args: &ListArgs) -> Result<()> {
    let cfg = config_dir();
    if let Some(a) = &args.archive {
        let a = expand_tilde(&path_str(a));
        if !a.is_file() {
            bail!("not found: {}", a.display());
        }
        return list_archive(&a, &cfg);
    }

    let project = resolve_project(&args.project)?;
    if args.local {
        let Some(project_dir) = find_project_dir(&cfg, &project) else {
            bail!("no Claude Code data found for {}", project.display());
        };
        log(format!("sessions in {}", project_dir.display()));
        for t in transcripts_in(&project_dir) {
            let file = t.file_name().unwrap().to_string_lossy().into_owned();
            let Some(sid) = session_id_of(&file) else {
                continue;
            };
            let info = scan_transcript(&t, &[]);
            let title = {
                let c = custom_title(&project_dir.join(&sid));
                if c.is_empty() { info.first_prompt } else { c }
            };
            log(format!(
                "  {}  {}  {:<8} {}",
                &sid[..8],
                DateTime::<Local>::from(mtime(&t)).format("%Y-%m-%d %H:%M"),
                human(fs::metadata(&t)?.len()),
                title.chars().take(70).collect::<String>()
            ));
        }
        return Ok(());
    }

    let out_root = backup_root(&args.out);
    let own_dir = out_root.join(encode_path(&path_str(&project)));
    let own = archives_in(&own_dir);
    if args.all || own.is_empty() {
        let all = all_archives(&out_root);
        if all.is_empty() {
            log(format!("no backups under {}", out_root.display()));
            return Ok(());
        }
        if args.all {
            log(format!("all backups under {}", out_root.display()));
        } else {
            log(format!(
                "no backups for {}; all backups under {}:",
                project.display(),
                out_root.display()
            ));
        }
        print_archives(&all, true);
        return Ok(());
    }
    log(format!(
        "backups for {} in {}",
        project.display(),
        own_dir.display()
    ));
    print_archives(&own, false);
    Ok(())
}

fn list_archive(archive: &Path, cfg: &Path) -> Result<()> {
    let m = read_manifest(archive)?;
    log(format!("archive      {}", archive.display()));
    log(format!("project      {}", m.project_path));
    log(format!("created      {} on {}", m.created_at, m.hostname));
    log(format!(
        "memory       {}",
        if m.memory_included {
            "included"
        } else {
            "none"
        }
    ));
    log(format!(
        "plans        {}",
        if m.plans.is_empty() {
            "none".to_string()
        } else {
            m.plans.join(", ")
        }
    ));
    log(format!("task lists   {}", m.tasks.len()));
    log(format!("history      {} prompt lines", m.history_lines));
    log(format!(
        "config entry {}",
        if m.claude_json_entry_included {
            "included"
        } else {
            "none"
        }
    ));
    log("sessions:");
    let local = find_project_dir(cfg, Path::new(&m.project_path));
    for s in &m.sessions {
        let present = match &local {
            Some(d) if d.join(&s.file).is_file() => "present",
            _ => "missing locally",
        };
        let mut extras = Vec::new();
        if !s.plans.is_empty() {
            extras.push(format!(
                "{} plan{}",
                s.plans.len(),
                if s.plans.len() > 1 { "s" } else { "" }
            ));
        }
        if s.file_history {
            extras.push("checkpoints".to_string());
        }
        log(format!(
            "  {}  {}  {:<8} {:<16} {}{}",
            &s.id[..8],
            s.modified
                .chars()
                .take(16)
                .collect::<String>()
                .replace('T', " "),
            human(s.size),
            present,
            s.label().chars().take(60).collect::<String>(),
            if extras.is_empty() {
                String::new()
            } else {
                format!("  [{}]", extras.join(", "))
            }
        ));
    }
    Ok(())
}
