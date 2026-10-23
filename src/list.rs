use crate::backup::{default_out, resolve_project};
use crate::claude::*;
use crate::manifest::read_manifest;
use anyhow::{Result, bail};
use chrono::{DateTime, Local};
use std::fs;
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

    let out_root = args
        .out
        .clone()
        .map(|p| expand_tilde(&path_str(&p)))
        .unwrap_or_else(default_out);
    let out_dir = out_root.join(encode_path(&path_str(&project)));
    let mut archives: Vec<PathBuf> = fs::read_dir(&out_dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file() && path_str(p).ends_with(".tar.gz"))
                .collect()
        })
        .unwrap_or_default();
    archives.sort_by_key(|p| std::cmp::Reverse(mtime(p)));
    if archives.is_empty() {
        log(format!(
            "no backups for {} in {}",
            project.display(),
            out_root.display()
        ));
        return Ok(());
    }
    log(format!("backups for {}", project.display()));
    for a in archives {
        match read_manifest(&a) {
            Ok(m) => log(format!(
                "  {}  {:<9} {:>3} sessions  {:>2} plans  {}",
                m.created_at.chars().take(19).collect::<String>(),
                human(fs::metadata(&a)?.len()),
                m.sessions.len(),
                m.plans.len(),
                a.display()
            )),
            Err(_) => log(format!("  (unreadable)  {}", a.display())),
        }
    }
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
