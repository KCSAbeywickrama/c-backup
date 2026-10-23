//! The `manifest.json` stored first in every archive. Field names match the Python
//! implementation so archives are interchangeable.

use anyhow::{Context, Result, bail};
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::Read;
use std::path::Path;

pub const MANIFEST_FORMAT: u32 = 1;
pub const MANIFEST_NAME: &str = "manifest.json";

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SessionEntry {
    pub id: String,
    pub file: String,
    pub size: u64,
    pub modified: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub first_prompt: String,
    #[serde(default)]
    pub claude_version: String,
    #[serde(default)]
    pub plans: Vec<String>,
    #[serde(default)]
    pub tasks: Vec<String>,
    #[serde(default)]
    pub file_history: bool,
}

impl SessionEntry {
    pub fn label(&self) -> &str {
        if self.title.is_empty() {
            &self.first_prompt
        } else {
            &self.title
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Manifest {
    pub format: u32,
    pub tool_version: String,
    pub created_at: String,
    pub hostname: String,
    pub config_dir: String,
    pub project_path: String,
    pub project_encoded: String,
    pub memory_root: String,
    #[serde(default)]
    pub memory_included: bool,
    #[serde(default)]
    pub plans: Vec<String>,
    #[serde(default)]
    pub tasks: Vec<String>,
    #[serde(default)]
    pub history_lines: usize,
    #[serde(default)]
    pub claude_json_entry_included: bool,
    pub sessions: Vec<SessionEntry>,
}

pub fn open_archive(archive: &Path) -> Result<tar::Archive<GzDecoder<File>>> {
    let f = File::open(archive).with_context(|| format!("cannot open {}", archive.display()))?;
    Ok(tar::Archive::new(GzDecoder::new(f)))
}

pub fn read_manifest(archive: &Path) -> Result<Manifest> {
    let mut tar = open_archive(archive)?;
    let entries = tar
        .entries()
        .with_context(|| format!("cannot read {}", archive.display()))?;
    for entry in entries {
        let mut entry = entry.with_context(|| format!("cannot read {}", archive.display()))?;
        if entry.path()?.as_os_str() == MANIFEST_NAME {
            let mut s = String::new();
            entry.read_to_string(&mut s)?;
            let m: Manifest = serde_json::from_str(&s).context("manifest.json is not valid")?;
            if m.format != MANIFEST_FORMAT {
                bail!(
                    "unsupported manifest format {} (this tool reads {})",
                    m.format,
                    MANIFEST_FORMAT
                );
            }
            return Ok(m);
        }
    }
    bail!(
        "{} is not a claude-backup archive (no manifest)",
        archive.display()
    )
}
