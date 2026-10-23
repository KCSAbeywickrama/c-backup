# c-backup

Back up and restore everything Claude Code keeps for one project folder. A command-line tool for Claude Code.

> Not affiliated with or endorsed by Anthropic. Claude is a trademark of Anthropic.

Claude Code stores transcripts, plans, memory and checkpoints under `~/.claude`,
keyed by the folder you launched it from. It has no built-in backup or restore,
and `cleanupPeriodDays` (default 30 days) silently deletes old sessions, plans
and checkpoints. This tool packs a project's data into one `.tar.gz` and puts
it back later, on the same machine or another one.

Written in Rust: one static binary, no runtime to install.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/KCSAbeywickrama/c-backup/main/install.sh | sh
```

Downloads a prebuilt binary for your platform, verifies its SHA-256 against the
release checksums, then installs `c-backup` and the short alias `cbackup`
into `~/.local/bin`. The two names are interchangeable. No Rust toolchain
needed.

Prebuilt binaries cover macOS (Apple Silicon and Intel) and Linux (x86_64 and
arm64, statically linked). Any other platform builds from source automatically,
which does need a Rust toolchain (1.88 or newer, [rustup.rs](https://rustup.rs)).

```sh
BINDIR=/usr/local/bin ./install.sh   # choose another bin dir
VERSION=v0.1.0 ./install.sh          # pin a specific release
FROM_SOURCE=1 ./install.sh           # skip the download, build locally
REPO=owner/name ./install.sh         # a different repository
```

`install.sh` downloads from `$REPO` (default `KCSAbeywickrama/c-backup`) and
refuses to install anything it cannot verify. Until that repository has a
release it falls through to building from source, so `./install.sh` works
either way.

If the script warns that the bin dir is not on your PATH, add it, for example
to `~/.zshrc`:

```sh
export PATH="$HOME/.local/bin:$PATH"
```

Uninstall:

```sh
./uninstall.sh
```

### Working on the code

`install-dev.sh` points `~/.local/bin/c-backup` at `target/release/c-backup`
with a symlink instead of copying it, so `cargo build --release` updates the
installed command in place while you work:

```sh
./install-dev.sh
```

`cargo clean` removes what the symlink points at, so rebuild or re-run it after
one. Releases are cut by pushing a `v*` tag; the workflow in `.github/workflows/`
builds the four targets, writes `SHA256SUMS`, and publishes them.

## Use

```sh
cd ~/work/my-project
cbackup                    # backup (default command)
cbackup --here             # backup into ./.claude-backup/ and keep doing so from now on
cbackup list               # backups of this folder (or all backups, if it has none)
cbackup list --all         # every backup under the backup root
cbackup list <archive>     # what is inside one archive
cbackup list --local       # sessions currently in ~/.claude for this folder
cbackup restore            # newest backup of this folder; asks which one if there are several
cbackup restore <archive>  # a specific archive
```

`restore` without an archive looks in the folder's own `.claude-backup/`
first, then in that folder's backups under the backup root. One match is used
as is; several are listed and you pick. When the folder has none at all, it
lists every backup under the backup root, grouped by project, and asks which
one to use. An archive can also be given as a bare file name.

### Keeping backups inside the project

`cbackup --here` writes the archive to `<project>/.claude-backup/`
instead. Once that folder exists, plain `cbackup` keeps using it for
that project; its presence is the opt-in. The backup then travels with the
folder: rename it, move it, copy it to another machine, and `cbackup
restore` run inside it finds the archive without any path.

In a git repository the tool adds `.claude-backup/` to `.git/info/exclude`,
which ignores it locally without touching a tracked file, and warns if the
folder still shows up as unignored. Transcripts contain file contents, command
output and anything a tool printed, so they must never be committed; keep the
folder out of anything that syncs or publishes the project. A backup kept only
inside the folder also disappears with it, so for insurance against deleting
the project use the default location or both.

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

When the folder you restore into differs from the one the archive was made in,
`restore` prints both paths and asks whether to rewrite the old path inside the
restored transcripts, memory and plans (see below). For non-interactive use,
such as a hook or a script, there is no terminal to ask on, so nothing is
rewritten unless you pass `--rewrite-paths` or `--no-rewrite-paths`.

Restore is non-destructive by default: files that already exist are kept, and
prompt-history lines are appended only if not already present. Run it twice
and the second run writes nothing. `--force` refuses to run while a Claude Code
session is open on that project.

The `~/.claude.json` entry is not restored unless you pass `--with-config`,
because it contains `hasTrustDialogAccepted` and `allowedTools`. Restoring it on
a new machine skips the trust prompt for that folder, which you should decide
consciously.

### Renamed or moved project

```sh
cd A && cbackup                  # or: cbackup --here
mv A B
cd B && cbackup restore          # add --with-config to skip the trust prompt again
claude --resume
```

With `--here` the archive moved along inside `B/.claude-backup/` and `restore`
takes it directly. Otherwise B has no backups of its own yet, so `restore`
lists every backup and asks which one to use; pick A's. Either way it then
notices the folder changed and asks:

```
This archive was made for  /work/A
You are restoring it to    /work/B
Rewrite that path inside the restored transcripts, memory and plans so that
resumed sessions and /rewind refer to the new folder? [Y/n]
```

Answer `y` and every recorded absolute path under A becomes the same path
under B: the `cwd` on each record, the file paths `/rewind` checkpoints are
keyed by, and any mention in memory notes or plans. Only whole paths are
touched, so `/work/A` never turns `/work/A-other` into something else. The
archive itself is never modified. Answer `n` and the conversation still
resumes, but `/rewind` into pre-rename checkpoints is not supported.

Files land under the encoded name of the new path and prompt history is
rewritten to point at it. The old `projects/<encoded-A>` data stays until you
run `claude purge <old path>`. The same flow restores a project onto another
machine; the ask appears there too whenever the path differs.

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
src/manifest.rs  manifest.json schema (first entry of every archive)
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
- Hooks receive `transcript_path`, so a `SessionEnd` hook can call `cbackup` automatically:

```json
{
  "hooks": {
    "SessionEnd": [
      { "hooks": [ { "type": "command", "command": "cbackup --no-file-history --keep 10" } ] }
    ]
  }
}
```

## License

MIT
