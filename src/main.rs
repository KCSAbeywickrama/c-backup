//! claude-backup: back up and restore Claude Code session data for one project.

mod backup;
mod claude;
mod list;
mod manifest;
mod restore;

use clap::{Parser, Subcommand};

const ABOUT: &str = "\
Back up and restore Claude Code session data for one project.

Run it from a project folder. It finds everything Claude Code keeps for that
folder under ~/.claude (or $CLAUDE_CONFIG_DIR) and packs it into one archive:

  transcripts      projects/<encoded-cwd>/<session>.jsonl (+ subagents, tool-results)
  memory           projects/<encoded-git-root>/memory/
  plans            plans/<slug>.md referenced by those sessions
  checkpoints      file-history/<session>/      (what /rewind uses)
  session env      session-env/<session>/
  task lists       tasks/<id>/ referenced by those sessions
  prompt history   history.jsonl lines for this project
  project config   the projects[\"<path>\"] entry of ~/.claude.json

`restore` puts them back, non-destructively by default (existing files are
kept unless --force). With no subcommand, `backup` runs.";

#[derive(Parser, Debug)]
#[command(name = "claude-backup", version, about = "Back up and restore Claude Code session data for one project", long_about = ABOUT)]
#[command(args_conflicts_with_subcommands = true)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
    #[command(flatten)]
    backup: backup::BackupArgs,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Archive this folder's Claude Code data (default command)
    Backup(backup::BackupArgs),
    /// Put an archive back into ~/.claude for this folder
    Restore(restore::RestoreArgs),
    /// List backups of this folder, or the contents of an archive
    List(list::ListArgs),
}

fn main() {
    let cli = Cli::parse();
    let result = match &cli.cmd {
        Some(Cmd::Backup(a)) => backup::run(a),
        Some(Cmd::Restore(a)) => restore::run(a),
        Some(Cmd::List(a)) => list::run(a),
        None => backup::run(&cli.backup),
    };
    if let Err(e) = result {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
