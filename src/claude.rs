//! Where Claude Code keeps things, and how to read the bits we need.

use regex::bytes::Regex;
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;
use std::time::SystemTime;

static UUID_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}").unwrap()
});
static SLUG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""slug":"([^"\\]{1,200})""#).unwrap());
static PLAN_PATH_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""planFilePath":"([^"\\]{1,500})""#).unwrap());

/// Print a line to stdout; a closed pipe ends the program quietly instead of panicking.
pub fn log(line: impl AsRef<str>) {
    let mut out = io::stdout().lock();
    if writeln!(out, "{}", line.as_ref()).is_err() || out.flush().is_err() {
        std::process::exit(0);
    }
}

pub fn warn(msg: impl AsRef<str>) {
    eprintln!("warning: {}", msg.as_ref());
}

/// True when both stdin and stdout are a terminal, so a question can be asked.
pub fn can_ask() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal()
}

/// Ask a yes/no question on the terminal. Enter takes the default; EOF too.
pub fn confirm(question: &str, default_yes: bool) -> bool {
    let hint = if default_yes { "[Y/n]" } else { "[y/N]" };
    loop {
        print!("{question} {hint} ");
        let _ = io::stdout().flush();
        let mut line = String::new();
        match io::stdin().read_line(&mut line) {
            Ok(0) | Err(_) => return default_yes,
            Ok(_) => {}
        }
        match line.trim().to_ascii_lowercase().as_str() {
            "" => return default_yes,
            "y" | "yes" => return true,
            "n" | "no" => return false,
            _ => log("please answer y or n"),
        }
    }
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

pub fn expand_tilde(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None if p == "~" => home(),
        None => PathBuf::from(p),
    }
}

fn config_override() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// `~/.claude`, or `$CLAUDE_CONFIG_DIR` when set.
pub fn config_dir() -> PathBuf {
    config_override().unwrap_or_else(|| home().join(".claude"))
}

/// `~/.claude.json`, or `<CLAUDE_CONFIG_DIR>/.claude.json` when that variable is set.
pub fn claude_json_path() -> PathBuf {
    match config_override() {
        Some(d) => d.join(".claude.json"),
        None => home().join(".claude.json"),
    }
}

/// Folder inside a project that holds its own backups (`--here`).
pub const LOCAL_DIR: &str = ".claude-backup";

pub fn local_backup_dir(project: &Path) -> PathBuf {
    project.join(LOCAL_DIR)
}

/// `.git/info/exclude` for the repository containing `path`, also inside worktrees.
pub fn git_exclude_path(path: &Path) -> Option<PathBuf> {
    let out = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--git-path", "info/exclude"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let p = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    Some(if p.is_absolute() { p } else { path.join(p) })
}

/// Whether git ignores `rel` (relative to `path`).
pub fn git_ignores(path: &Path, rel: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["check-ignore", "-q", rel])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Claude Code's project-directory name: every non-alphanumeric char becomes '-'.
pub fn encode_path(path: &str) -> String {
    path.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

pub fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

pub fn git_root(path: &Path) -> Option<PathBuf> {
    let out = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        return None;
    }
    Some(fs::canonicalize(&s).unwrap_or_else(|_| PathBuf::from(s)))
}

fn settings(cfg: &Path) -> Value {
    fs::read(cfg.join("settings.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null)
}

pub fn plans_dir(cfg: &Path, project_root: &Path) -> PathBuf {
    if let Some(custom) = settings(cfg).get("plansDirectory").and_then(Value::as_str) {
        return project_root.join(expand_tilde(custom));
    }
    cfg.join("plans")
}

pub fn memory_dir(cfg: &Path, memory_root: &Path) -> PathBuf {
    if let Some(custom) = settings(cfg)
        .get("autoMemoryDirectory")
        .and_then(Value::as_str)
    {
        return memory_root.join(expand_tilde(custom));
    }
    cfg.join("projects")
        .join(encode_path(&path_str(memory_root)))
        .join("memory")
}

pub fn session_id_of(filename: &str) -> Option<String> {
    UUID_RE.find(filename).map(|m| m.as_str().to_string())
}

pub fn mtime(p: &Path) -> SystemTime {
    fs::metadata(p)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

/// `*.jsonl` files in a project directory, newest first.
pub fn transcripts_in(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "jsonl"))
                .collect()
        })
        .unwrap_or_default();
    v.sort_by_key(|p| std::cmp::Reverse(mtime(p)));
    v
}

/// Locate `projects/<encoded>` for a path. Falls back to scanning transcripts for `cwd`,
/// which covers the hashed names Claude Code uses for very long paths.
pub fn find_project_dir(cfg: &Path, project: &Path) -> Option<PathBuf> {
    let projects = cfg.join("projects");
    let direct = projects.join(encode_path(&path_str(project)));
    if direct.is_dir() {
        return Some(direct);
    }
    let needle = format!(
        "\"cwd\":{}",
        serde_json::to_string(&path_str(project)).ok()?
    );
    for d in fs::read_dir(&projects).ok()?.flatten() {
        let d = d.path();
        if !d.is_dir() {
            continue;
        }
        for t in transcripts_in(&d).into_iter().take(3) {
            let mut head = Vec::new();
            if let Ok(f) = File::open(&t) {
                let _ = f.take(64 * 1024).read_to_end(&mut head);
            }
            if memchr::memmem::find(&head, needle.as_bytes()).is_some() {
                return Some(d);
            }
        }
    }
    None
}

#[derive(Default)]
pub struct ScanInfo {
    pub slugs: Vec<String>,
    pub plan_paths: Vec<String>,
    pub tasks: Vec<String>,
    pub first_prompt: String,
    pub version: String,
}

/// Pull plan slugs, plan paths, referenced task lists, version and the first prompt out of a transcript.
pub fn scan_transcript(path: &Path, task_ids: &[String]) -> ScanInfo {
    let Ok(data) = fs::read(path) else {
        return ScanInfo::default();
    };

    let slugs: BTreeSet<String> = SLUG_RE
        .captures_iter(&data)
        .map(|c| String::from_utf8_lossy(&c[1]).into_owned())
        .filter(|s| !UUID_RE.is_match(s))
        .collect();
    let plan_paths: BTreeSet<String> = PLAN_PATH_RE
        .captures_iter(&data)
        .map(|c| String::from_utf8_lossy(&c[1]).into_owned())
        .collect();
    let tasks = task_ids
        .iter()
        .filter(|t| memchr::memmem::find(&data, t.as_bytes()).is_some())
        .cloned()
        .collect();

    let mut first_prompt = String::new();
    let mut version = String::new();
    for line in data.split(|b| *b == b'\n').take(400) {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let Ok(rec) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        if version.is_empty()
            && let Some(v) = rec.get("version").and_then(Value::as_str)
        {
            version = v.to_string();
        }
        if first_prompt.is_empty()
            && rec.get("type").and_then(Value::as_str) == Some("user")
            && !rec
                .get("isSidechain")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            && let Some(content) = rec.pointer("/message/content").and_then(Value::as_str)
            && !content.starts_with('<')
        {
            first_prompt = content
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(120)
                .collect();
        }
        if !first_prompt.is_empty() && !version.is_empty() {
            break;
        }
    }
    ScanInfo {
        slugs: slugs.into_iter().collect(),
        plan_paths: plan_paths.into_iter().collect(),
        tasks,
        first_prompt,
        version,
    }
}

pub fn custom_title(session_dir: &Path) -> String {
    fs::read(session_dir.join("custom-title.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| {
            v.get("customTitle")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default()
}

fn pid_alive(pid: u64) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// PIDs of interactive Claude Code processes currently open on this project.
pub fn running_sessions_for(cfg: &Path, project: &Path) -> Vec<u64> {
    let Ok(rd) = fs::read_dir(cfg.join("sessions")) else {
        return Vec::new();
    };
    let want = path_str(project);
    rd.flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .filter_map(|p| fs::read(p).ok())
        .filter_map(|b| serde_json::from_slice::<Value>(&b).ok())
        .filter(|rec| rec.get("cwd").and_then(Value::as_str) == Some(want.as_str()))
        .filter_map(|rec| rec.get("pid").and_then(Value::as_u64))
        .filter(|pid| pid_alive(*pid))
        .collect()
}

pub fn human(n: u64) -> String {
    let mut v = n as f64;
    for unit in ["B", "KB", "MB", "GB"] {
        if v < 1024.0 || unit == "GB" {
            return if unit == "B" {
                format!("{v:.0} {unit}")
            } else {
                format!("{v:.1} {unit}")
            };
        }
        v /= 1024.0;
    }
    unreachable!()
}

/// Only regular files and directories are archived; no symlinks, sockets or lock files.
pub fn wanted_in_archive(entry: &walkdir::DirEntry) -> bool {
    let ft = entry.file_type();
    if ft.is_symlink() || !(ft.is_file() || ft.is_dir()) {
        return false;
    }
    entry.file_name() != ".lock"
}

/// A member name is safe when it is relative and has no `.` or `..` components.
pub fn safe_member(name: &Path) -> bool {
    !name.as_os_str().is_empty()
        && name
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}
