#!/usr/bin/env sh
# Remove the claude-backup binaries installed by install.sh or install-dev.sh.
#
# Usage:
#   ./uninstall.sh
#   BINDIR=/usr/local/bin ./uninstall.sh
set -eu

BINDIR="${BINDIR:-$HOME/.local/bin}"
removed=0

for name in claude-backup cbackup; do
  target="$BINDIR/$name"
  if [ -e "$target" ] || [ -L "$target" ]; then
    rm -f "$target"
    echo "Removed: $target"
    removed=$((removed + 1))
  fi
done

if [ "$removed" -eq 0 ]; then
  echo "Nothing to remove in $BINDIR"
  echo "(If you installed elsewhere, re-run with BINDIR=/that/dir ./uninstall.sh)"
fi

echo
echo "Your backups are untouched. Build artifacts too; to drop them: cargo clean"
