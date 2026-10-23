# claude-backup

Back up and restore everything Claude Code keeps for one project folder.

Claude Code stores transcripts, plans, memory and checkpoints under `~/.claude`,
keyed by the folder you launched it from. It has no built-in backup or restore,
and `cleanupPeriodDays` (default 30 days) silently deletes old sessions, plans
and checkpoints. This tool packs a project's data into one `.tar.gz` and puts
it back later, on the same machine or another one.

Written in Rust. A single-file Python version with the same commands and the
same archive format lives on the `python` branch; archives made by either can
be restored by the other.

## Install

```sh
cargo install --path .            # puts claude-backup on ~/.cargo/bin
# or
cargo build --release && cp target/release/claude-backup /usr/local/bin/
```

## Use

```sh
cd ~/work/my-project
claude-backup                    # backup (default command)
claude-backup list               # backups of this folder
claude-backup list <archive>     # what is inside one archive
claude-backup list --local       # sessions currently in ~/.claude for this folder
claude-backup restore <archive>  # put it back for this folder
```

Archives go to `~/claude-backups/<encoded-project-path>/<name>-<timestamp>.tar.gz`
(override with `--out DIR` or `CLAUDE_BACKUP_DIR`). They are created with mode
`0600`. Transcripts are plaintext and include file contents, command output and
anything a tool printed, so treat the archive as confidential.

### What goes in

| Archive path | Source under `~/.claude` | Purpose |
|---|---|---|
| `project/<session>.jsonl` | `projects/<encoded-cwd>/` | conversation transcripts (`--resume`) |
| `project/<session>/` | same | subagent transcripts, large tool results, custom title |
| `memory/` | `projects/<encoded-git-root>/memory/` | auto memory (`MEMORY.md` and notes) |
| `plans/<slug>.md` | `plans/` | plan-mode files referenced by those sessions |
| `file-history/<session>/` | `file-history/` | `/rewind` checkpoints |
| `session-env/<session>/` | `session-env/` | per-session environment |
| `tasks/<id>/` | `tasks/` | task lists referenced by those sessions |
| `history.jsonl` | `history.jsonl` | prompt history lines for this project |
| `claude-json-project.json` | `~/.claude.json` → `projects["<path>"]` | trust, allowed tools, MCP settings |

Memory is keyed by git root, transcripts by working directory. The tool handles
both, so running it from a subfolder of a repo still picks up the repo's memory.
`CLAUDE_CONFIG_DIR`, `plansDirectory` and `autoMemoryDirectory` are honoured.

### Backup options

```
--project PATH       folder to back up (default: current directory)
--out DIR            backup root
--sessions a1b2,c3d4 only these session id prefixes
--since DAYS         only sessions modified in the last DAYS days
--no-file-history    skip /rewind checkpoints (much smaller)
--keep N             delete all but the newest N archives of this project
--dry-run
```

### Restore options

```
--project PATH   restore for this folder (default: current directory)
--original       restore to the path recorded in the archive
--sessions ...   only these session id prefixes
--force          overwrite files that already exist
--with-config    also write the ~/.claude.json project entry
--dry-run
-v               print every file
```

Restore is non-destructive by default: files that already exist are kept, and
prompt-history lines are appended only if not already present. Run it twice
and the second run writes nothing. `--force` refuses to run while a Claude Code
session is open on that project.

The `~/.claude.json` entry is not restored unless you pass `--with-config`,
because it contains `hasTrustDialogAccepted` and `allowedTools`. Restoring it on
a new machine skips the trust prompt for that folder, which you should decide
consciously.

### Restoring to a different folder or machine

Run `restore` from the new folder (or pass `--project`). Files land under the
encoded name of the new path and prompt history is rewritten to point at it.
Two things keep their original absolute paths because they are inside the
transcripts: references to plan files and `/rewind` checkpoints. The
conversation resumes fine; `/rewind` into pre-backup checkpoints may not work
when the folder moved.

Then:

```sh
cd <folder> && claude --resume
```

Claude Code's transcript format is internal and can change between versions.
A backup restored under a much newer version may not load.

## How it finds things

Project directory name = the absolute path with every non-alphanumeric
character replaced by `-` (`/Users/you/app` → `-Users-you-app`). If that does
not exist (very long paths get a hashed name), it scans `projects/` for a
transcript whose `cwd` matches. Plans are found through the `slug` and
`planFilePath` fields inside transcripts.

## Layout

```
src/main.rs      CLI (clap)
src/claude.rs    where Claude Code keeps things; path encoding; transcript scanning
src/manifest.rs  manifest.json schema, shared with the python branch
src/backup.rs    backup command
src/restore.rs   restore command
src/list.rs      list command
```

## Related Claude Code features (as of 2.1.x)

- `/export [file]` writes the current conversation as readable text. Not re-importable.
- `claude --resume` / `/resume` reopen sessions from `~/.claude/projects/`. `--resume` also accepts an absolute path to a `.jsonl`.
- `/cd <path>` moves a running session to another folder's project storage.
- `cleanupPeriodDays` in `settings.json` controls deletion of old transcripts, plans and checkpoints. Raise it to keep things longer; it is retention, not backup.
- `claude purge [path]` deletes a project's data. The opposite of this tool.
- `~/.claude/backups/` holds copies of `~/.claude.json` only, never transcripts.
- Hooks receive `transcript_path`, so a `SessionEnd` hook can call `claude-backup` automatically:

```json
{
  "hooks": {
    "SessionEnd": [
      { "hooks": [ { "type": "command", "command": "claude-backup --no-file-history --keep 10" } ] }
    ]
  }
}
```
