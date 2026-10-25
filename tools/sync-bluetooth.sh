#!/usr/bin/env bash
# Regenerates bluetooth/, the Bluetooth (standard) edition, from the root.
#
# The edition is the root's shared tree (every file git tracks except
# bluetooth/, .github/, docs/, mockups/, the root README and this script and
# its test), with
# bluetooth/edition/patches/*.patch applied in order and
# bluetooth/edition/files/ laid over the result. bluetooth/edition/ itself
# is the edition's source and is never touched.
#
# Usage:
#   bash tools/sync-bluetooth.sh           write bluetooth/ (and delete there what the root no longer has)
#   bash tools/sync-bluetooth.sh --check   fail with a diff if bluetooth/ is not what the first would write
#
# Only tracked files travel: a new root file is shared once it is added
# (git add), and ignored or untracked files never are.
set -euo pipefail
export LC_ALL=C

MODE=sync
case "${1:-}" in
  '') ;;
  --check) MODE=check ;;
  *) echo "usage: $0 [--check]" >&2; exit 2 ;;
esac

cd "$(dirname "$0")/.."
ROOT="$PWD"
OUT=bluetooth
EDITION="$OUT/edition"

# Not shared: the edition itself, CI, the docs and the design mockups, the
# root README (the edition's own notes are bluetooth/edition/README.md), and
# this script and its test, which only make sense at the root.
excluded() {
  case "$1" in
    "$OUT"/*|.github/*|docs/*|mockups/*|README.md|tools/sync-bluetooth.sh|tools/test-sync-bluetooth.sh) return 0 ;;
    *) return 1 ;;
  esac
}

WORK="$(mktemp -d "${TMPDIR:-/tmp}/bombechat-sync.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
BUILT="$WORK/built"
mkdir -p "$BUILT"

# 1. the shared tree, as the root tracks it (a file deleted but not yet
#    staged is already gone, so it is not copied)
while IFS= read -r -d '' f; do
  excluded "$f" && continue
  [ -f "$f" ] || continue
  mkdir -p "$BUILT/$(dirname "$f")"
  cp "$f" "$BUILT/$f"
done < <(git ls-files -z --cached)

# 2. the edition's patches, in order. Each may touch only files listed in
#    PATCHED, and every file listed there must have a patch.
PATCHED_LIST="$WORK/patched"
: > "$PATCHED_LIST"
if [ -f "$EDITION/PATCHED" ]; then
  { grep -v -e '^#' -e '^[[:space:]]*$' "$EDITION/PATCHED" || true; } | sort -u > "$PATCHED_LIST"
fi
TOUCHED="$WORK/touched"
: > "$TOUCHED"
for p in "$EDITION"/patches/*.patch; do
  [ -f "$p" ] || continue
  sed -n 's#^+++ b/\([^[:space:]]*\).*#\1#p' "$p" >> "$TOUCHED"
  # the build directory is outside any repository, so git apply works as patch(1) does
  if ! (cd "$BUILT" && GIT_CEILING_DIRECTORIES="$WORK" git apply --whitespace=nowarn "$ROOT/$p"); then
    echo "sync-bluetooth: $p no longer applies to the root; update it" >&2
    exit 1
  fi
done
sort -u -o "$TOUCHED" "$TOUCHED"
if ! cmp -s "$TOUCHED" "$PATCHED_LIST"; then
  echo "sync-bluetooth: $EDITION/PATCHED must list exactly the files the patches change" >&2
  diff "$PATCHED_LIST" "$TOUCHED" | sed -n 's/^< /  listed but not patched: /p; s/^> /  patched but not listed: /p' >&2
  exit 1
fi

# 3. the edition's own files, over everything
if [ -d "$EDITION/files" ]; then
  cp -R "$EDITION/files/." "$BUILT/"
fi

# What bluetooth/ holds now, outside edition/: tracked files and untracked
# ones git doesn't ignore (bluetooth/target, node_modules and checks are ignored).
CURRENT="$WORK/current"
WANTED="$WORK/wanted"
git ls-files -z --cached --others --exclude-standard -- "$OUT" \
  | tr '\0' '\n' | { grep -v "^$EDITION/" || true; } | sed "s#^$OUT/##" \
  | while IFS= read -r f; do if [ -f "$OUT/$f" ]; then printf '%s\n' "$f"; fi; done | sort -u > "$CURRENT"
(cd "$BUILT" && find . -type f | sed 's#^\./##' | sort) > "$WANTED"

if [ "$MODE" = check ]; then
  status=0
  while IFS= read -r f; do
    echo "sync-bluetooth: $OUT/$f is missing" >&2; status=1
  done < <(comm -23 "$WANTED" "$CURRENT")
  while IFS= read -r f; do
    echo "sync-bluetooth: $OUT/$f is in neither the root's shared tree nor $EDITION/files" >&2; status=1
  done < <(comm -13 "$WANTED" "$CURRENT")
  while IFS= read -r f; do
    if ! cmp -s "$BUILT/$f" "$OUT/$f"; then
      echo "sync-bluetooth: $OUT/$f differs from what the sync writes:" >&2
      diff -u -L "sync writes $f" -L "$OUT/$f" "$BUILT/$f" "$OUT/$f" >&2 || true
      status=1
    fi
  done < <(comm -12 "$WANTED" "$CURRENT")
  if [ "$status" -ne 0 ]; then
    echo "sync-bluetooth: $OUT/ is out of date: run bash tools/sync-bluetooth.sh, or make the change at the root or in $EDITION/" >&2
    exit 1
  fi
  echo "sync-bluetooth: $OUT/ is in sync"
  exit 0
fi

# sync: delete what the root no longer has, and the folders that leaves
# empty, then write what changed
comm -13 "$WANTED" "$CURRENT" | while IFS= read -r f; do
  rm -f "$OUT/$f"
  d="$(dirname "$OUT/$f")"
  while [ "$d" != "$OUT" ] && rmdir "$d" 2>/dev/null; do d="$(dirname "$d")"; done
done
while IFS= read -r f; do
  if ! cmp -s "$BUILT/$f" "$OUT/$f"; then
    mkdir -p "$OUT/$(dirname "$f")"
    cp "$BUILT/$f" "$OUT/$f"
  fi
done < "$WANTED"
echo "sync-bluetooth: wrote $OUT/ ($(wc -l < "$WANTED" | tr -d ' ') files)"
