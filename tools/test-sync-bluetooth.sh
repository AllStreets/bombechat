#!/usr/bin/env bash
# Tests tools/sync-bluetooth.sh on a small throwaway repository, then checks
# that this repository's bluetooth/ holds every shared root file.
#   bash tools/test-sync-bluetooth.sh
set -euo pipefail
export LC_ALL=C

HERE="$(cd "$(dirname "$0")/.." && pwd)"
T="$(mktemp -d "${TMPDIR:-/tmp}/bombechat-sync-test.XXXXXX")"
trap 'rm -rf "$T"' EXIT
FAILS=0
pass() { echo "ok   $1"; }
fail() { echo "FAIL $1" >&2; FAILS=$((FAILS + 1)); }
sync() { bash tools/sync-bluetooth.sh "$@" > "$T/out" 2>&1; }

cd "$T"
mkdir repo && cd repo
git init -q
mkdir -p tools app/ui crates/core docs mockups .github/workflows bluetooth/edition/patches bluetooth/edition/files/app/ui
cp "$HERE/tools/sync-bluetooth.sh" tools/
printf 'node_modules/\n/target\n' > .gitignore
printf 'shared\n' > app/ui/page.html
printf 'title = "airgapped"\nkeep = 1\n' > app/ui/conf.toml
printf ':root{--build:%s}\n' "'airgapped build'" > app/ui/edition.css
printf 'core\n' > crates/core/lib.rs
printf 'licence\n' > LICENSE
printf 'readme\n' > README.md
printf 'spec\n' > docs/spec.md
printf 'mock\n' > mockups/m.html
printf 'ci\n' > .github/workflows/ci.yml
mkdir node_modules && printf 'ignored\n' > node_modules/x.js
cat > bluetooth/edition/patches/0001-title.patch <<'EOF'
--- a/app/ui/conf.toml
+++ b/app/ui/conf.toml
@@ -1,2 +1,2 @@
-title = "airgapped"
+title = "standard"
 keep = 1
EOF
printf '# patched files\napp/ui/conf.toml\n' > bluetooth/edition/PATCHED
printf ':root{--build:%s}\n' "'standard build'" > bluetooth/edition/files/app/ui/edition.css
printf 'edition only\n' > bluetooth/edition/files/app/ui/radio.js
git add -A
printf 'untracked\n' > app/ui/untracked.txt

sync && pass "first sync runs" || { cat "$T/out"; fail "first sync runs"; }
git add -A bluetooth
sync --check && pass "--check passes right after a sync" || { cat "$T/out"; fail "--check passes right after a sync"; }

[ "$(cat bluetooth/app/ui/page.html)" = shared ] && [ -f bluetooth/crates/core/lib.rs ] && [ -f bluetooth/LICENSE ] && [ -f bluetooth/.gitignore ] \
  && pass "the shared tree is copied (LICENSE and .gitignore too)" || fail "the shared tree is copied"
[ ! -e bluetooth/docs ] && [ ! -e bluetooth/mockups ] && [ ! -e bluetooth/.github ] && [ ! -e bluetooth/README.md ] && [ ! -e bluetooth/bluetooth ] \
  && pass "docs, mockups, .github, README and bluetooth/ itself are not copied" || fail "exclusions are not copied"
[ ! -e bluetooth/app/ui/untracked.txt ] && [ ! -e bluetooth/node_modules ] \
  && pass "untracked and ignored files never travel" || fail "untracked and ignored files never travel"
grep -q 'title = "standard"' bluetooth/app/ui/conf.toml && grep -q 'title = "airgapped"' app/ui/conf.toml \
  && pass "patches apply to the copy, never the root" || fail "patches apply to the copy, never the root"
grep -q "standard build" bluetooth/app/ui/edition.css && grep -q "airgapped build" app/ui/edition.css && [ -f bluetooth/app/ui/radio.js ] \
  && pass "edition/files override the shared copy and add their own" || fail "edition/files override the shared copy"

# a file added at the root appears in the copy after a sync, and --check
# fails until then
printf 'new\n' > crates/core/new.rs
git add crates/core/new.rs
sync --check && fail "--check fails when the root gains a file" || pass "--check fails when the root gains a file"
grep -q "bluetooth/crates/core/new.rs is missing" "$T/out" || fail "--check names the missing file"
sync && [ "$(cat bluetooth/crates/core/new.rs)" = new ] && pass "a file added at the root appears after sync" || fail "a file added at the root appears after sync"
git add -A bluetooth
sync --check || fail "--check passes after syncing the new file"

# a root fix reaches the copy
printf 'shared, fixed\n' > app/ui/page.html
git add app/ui/page.html
sync --check && fail "--check fails when a root file changes" || pass "--check fails when a root file changes"
sync && [ "$(cat bluetooth/app/ui/page.html)" = "shared, fixed" ] && pass "a root change reaches the copy" || fail "a root change reaches the copy"
git add -A bluetooth

# a hand edit in the copy, outside edition/, fails --check with a diff
printf 'hand edit\n' >> bluetooth/crates/core/lib.rs
sync --check && fail "a hand edit in bluetooth/ fails --check" || pass "a hand edit in bluetooth/ fails --check"
grep -q '^+hand edit' "$T/out" && pass "--check shows the diff" || { cat "$T/out"; fail "--check shows the diff"; }
sync && [ "$(cat bluetooth/crates/core/lib.rs)" = core ] && pass "a sync undoes the hand edit" || fail "a sync undoes the hand edit"

# a file in neither place fails --check, and a sync deletes it
printf 'stray\n' > bluetooth/app/ui/stray.js
sync --check && fail "a file in neither place fails --check" || pass "a file in neither place fails --check"
grep -q "bluetooth/app/ui/stray.js is in neither" "$T/out" || fail "--check names the stray file"
sync && [ ! -e bluetooth/app/ui/stray.js ] && pass "a sync deletes the stray file" || fail "a sync deletes the stray file"

# a root file deleted is deleted in the copy, with the folder it leaves empty
git rm -qf crates/core/new.rs crates/core/lib.rs
sync --check && fail "--check fails when a root file is deleted" || pass "--check fails when a root file is deleted"
sync && [ ! -e bluetooth/crates/core/new.rs ] && [ ! -e bluetooth/crates/core ] && pass "a root file deleted is deleted in the copy" || fail "a root file deleted is deleted in the copy"
[ -f bluetooth/edition/PATCHED ] && [ -f bluetooth/edition/files/app/ui/radio.js ] && pass "edition/ is never touched" || fail "edition/ is never touched"
git add -A bluetooth
sync --check || { cat "$T/out"; fail "--check passes after the deletion"; }

# a patch that no longer applies, or that touches a file PATCHED doesn't list, fails
cp app/ui/conf.toml "$T/conf.toml"
printf 'title = "renamed"\nkeep = 1\n' > app/ui/conf.toml
git add app/ui/conf.toml
sync && fail "a patch that no longer applies fails the sync" || pass "a patch that no longer applies fails the sync"
cp "$T/conf.toml" app/ui/conf.toml && git add app/ui/conf.toml
printf '# patched files\n' > bluetooth/edition/PATCHED
sync && fail "a patch to a file PATCHED doesn't list fails" || pass "a patch to a file PATCHED doesn't list fails"
grep -q "patched but not listed: app/ui/conf.toml" "$T/out" || fail "the failure names the unlisted file"

# this repository: every tracked root file outside the exclusions is in bluetooth/
cd "$HERE"
missing=0
while IFS= read -r -d '' f; do
  case "$f" in bluetooth/*|.github/*|docs/*|mockups/*|README.md|tools/sync-bluetooth.sh|tools/test-sync-bluetooth.sh) continue ;; esac
  [ -f "$f" ] || continue
  [ -f "bluetooth/$f" ] || { echo "  not in bluetooth/: $f" >&2; missing=1; }
done < <(git ls-files -z --cached)
[ "$missing" -eq 0 ] && pass "bluetooth/ holds every shared root file" || fail "bluetooth/ holds every shared root file"

if [ "$FAILS" -ne 0 ]; then echo "$FAILS failed" >&2; exit 1; fi
echo "all passed"
