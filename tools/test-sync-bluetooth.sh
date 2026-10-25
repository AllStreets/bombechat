#!/usr/bin/env bash
# Tests tools/sync-bluetooth.sh on a small throwaway repository, then checks
# that this repository's bluetooth/ holds every shared root file.
#   bash tools/test-sync-bluetooth.sh
set -euo pipefail
export LC_ALL=C

HERE="$(cd "$(dirname "$0")/.." && pwd)"
T="$(mktemp -d "${TMPDIR:-/tmp}/bombechat-sync-test.XXXXXX")"
trap 'rm -rf "$T"' EXIT
PASSED=0
FAILED=0
# ok NAME CONDITION: CONDITION is shell, evaluated; every check prints one line
ok() {
  if eval "$2"; then echo "ok   $1"; PASSED=$((PASSED + 1))
  else echo "FAIL $1" >&2; [ -s "$T/out" ] && sed 's/^/     /' "$T/out" >&2; FAILED=$((FAILED + 1)); fi
}
sync() { bash tools/sync-bluetooth.sh "$@" > "$T/out" 2>&1; }
mode_x() { [ -x "$1" ]; }

cd "$T"
mkdir repo && cd repo
git init -q
mkdir -p tools app/ui crates/core docs mockups .github/workflows bluetooth/edition/patches bluetooth/edition/files/app/ui
cp "$HERE/tools/sync-bluetooth.sh" tools/
printf 'node_modules/\n/target\n' > .gitignore
printf 'shared\n' > app/ui/page.html
printf 'title = "airgapped"\nkeep = 1\n' > app/ui/conf.toml
printf 'one\ntwo\n' > "app/ui/two words.txt"
printf ':root{--build:%s}\n' "'airgapped build'" > app/ui/edition.css
printf 'core\n' > crates/core/lib.rs
printf '#!/bin/sh\necho run\n' > tools/run.sh && chmod 755 tools/run.sh
printf 'root lock\n' > Cargo.lock
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
# as diff -u writes it: the date after a tab, and a space in the path
printf -- '--- a/app/ui/two words.txt\t2026-10-25 12:00:00\n+++ b/app/ui/two words.txt\t2026-10-25 12:00:00\n@@ -1,2 +1,2 @@\n one\n-two\n+three\n' \
  > bluetooth/edition/patches/0002-space.patch
printf '# patched files\napp/ui/conf.toml\napp/ui/two words.txt\n' > bluetooth/edition/PATCHED
printf ':root{--build:%s}\n' "'standard build'" > bluetooth/edition/files/app/ui/edition.css
printf 'edition only\n' > bluetooth/edition/files/app/ui/radio.js
printf 'edition lock\n' > bluetooth/Cargo.lock
git add -A
printf 'untracked\n' > app/ui/untracked.txt

: > "$T/out"
ok "first sync runs" 'sync'
git add -A bluetooth
ok "--check passes right after a sync" 'sync --check'
ok "the shared tree is copied (LICENSE and .gitignore too)" \
  '[ "$(cat bluetooth/app/ui/page.html)" = shared ] && [ -f bluetooth/crates/core/lib.rs ] && [ -f bluetooth/LICENSE ] && [ -f bluetooth/.gitignore ]'
ok "docs, mockups, .github, README and bluetooth/ itself are not copied" \
  '[ ! -e bluetooth/docs ] && [ ! -e bluetooth/mockups ] && [ ! -e bluetooth/.github ] && [ ! -e bluetooth/README.md ] && [ ! -e bluetooth/bluetooth ]'
ok "untracked and ignored files never travel" '[ ! -e bluetooth/app/ui/untracked.txt ] && [ ! -e bluetooth/node_modules ]'
ok "patches apply to the copy, never the root" \
  'grep -q "title = \"standard\"" bluetooth/app/ui/conf.toml && grep -q "title = \"airgapped\"" app/ui/conf.toml'
ok "a patch to a path with a space applies and matches PATCHED" \
  '[ "$(tail -1 "bluetooth/app/ui/two words.txt")" = three ] && [ "$(tail -1 "app/ui/two words.txt")" = two ]'
ok "edition/files override the shared copy and add their own" \
  'grep -q "standard build" bluetooth/app/ui/edition.css && grep -q "airgapped build" app/ui/edition.css && [ -f bluetooth/app/ui/radio.js ]'
ok "an executable root file is executable in the copy" 'mode_x bluetooth/tools/run.sh && ! mode_x bluetooth/app/ui/page.html'

# a file added at the root appears in the copy after a sync, and --check fails until then
printf 'new\n' > crates/core/new.rs
git add crates/core/new.rs
ok "--check fails when the root gains a file" '! sync --check'
ok "--check names the missing file" 'grep -q "bluetooth/crates/core/new.rs is missing" "$T/out"'
ok "a file added at the root appears after sync" 'sync && [ "$(cat bluetooth/crates/core/new.rs)" = new ]'
git add -A bluetooth
ok "--check passes after syncing the new file" 'sync --check'

# a root fix reaches the copy
printf 'shared, fixed\n' > app/ui/page.html
git add app/ui/page.html
ok "--check fails when a root file changes" '! sync --check'
ok "a root change reaches the copy" 'sync && [ "$(cat bluetooth/app/ui/page.html)" = "shared, fixed" ]'
git add -A bluetooth

# a hand edit in the copy, outside edition/, fails --check with a diff
printf 'hand edit\n' >> bluetooth/crates/core/lib.rs
ok "a hand edit in bluetooth/ fails --check" '! sync --check'
ok "--check shows the diff" 'grep -q "^+hand edit" "$T/out"'
ok "a sync undoes the hand edit" 'sync && [ "$(cat bluetooth/crates/core/lib.rs)" = core ]'

# a mode that differs and nothing else: --check fails, a sync restores it
chmod a-x bluetooth/tools/run.sh
ok "--check fails when only the executable bit is lost" '! sync --check'
ok "--check says the file should be executable" 'grep -q "bluetooth/tools/run.sh should be executable" "$T/out"'
ok "a sync restores the lost executable bit" 'sync && mode_x bluetooth/tools/run.sh && sync --check'
chmod a+x bluetooth/app/ui/page.html
ok "--check fails when only an executable bit is gained" '! sync --check'
ok "--check says the file should not be executable" 'grep -q "bluetooth/app/ui/page.html should be not executable" "$T/out"'
ok "a sync clears the gained executable bit" 'sync && ! mode_x bluetooth/app/ui/page.html && sync --check'

# a file in neither place fails --check, and a sync deletes it
printf 'stray\n' > bluetooth/app/ui/stray.js
ok "a file in neither place fails --check" '! sync --check'
ok "--check names the stray file" 'grep -q "bluetooth/app/ui/stray.js is in neither" "$T/out"'
ok "a sync deletes the stray file" 'sync && [ ! -e bluetooth/app/ui/stray.js ]'

# bluetooth/Cargo.lock is the edition's: never written, never checked
ok "the edition's Cargo.lock survives a sync" '[ "$(cat bluetooth/Cargo.lock)" = "edition lock" ]'
printf 'root lock, updated\n' > Cargo.lock
git add Cargo.lock
ok "a root Cargo.lock change leaves --check passing" 'sync --check'
ok "a root Cargo.lock change never reaches the edition's" 'sync && [ "$(cat bluetooth/Cargo.lock)" = "edition lock" ]'
printf 'edition lock, updated by cargo\n' > bluetooth/Cargo.lock
ok "--check ignores the edition's own Cargo.lock" 'sync --check'
ok "a sync leaves the edition's Cargo.lock as it is" 'sync && [ "$(cat bluetooth/Cargo.lock)" = "edition lock, updated by cargo" ]'
git add -A bluetooth

# a root file deleted is deleted in the copy, with the folder it leaves empty
git rm -qf crates/core/new.rs crates/core/lib.rs
ok "--check fails when a root file is deleted" '! sync --check'
ok "a root file deleted is deleted in the copy" 'sync && [ ! -e bluetooth/crates/core/new.rs ] && [ ! -e bluetooth/crates/core ]'
ok "edition/ is never touched" '[ -f bluetooth/edition/PATCHED ] && [ -f bluetooth/edition/files/app/ui/radio.js ]'
git add -A bluetooth
ok "--check passes after the deletion" 'sync --check'

# a patch that no longer applies, or that touches a file PATCHED doesn't list, fails
cp app/ui/conf.toml "$T/conf.toml"
printf 'title = "renamed"\nkeep = 1\n' > app/ui/conf.toml
git add app/ui/conf.toml
ok "a patch that no longer applies fails the sync" '! sync'
cp "$T/conf.toml" app/ui/conf.toml && git add app/ui/conf.toml
printf '# patched files\napp/ui/two words.txt\n' > bluetooth/edition/PATCHED
ok "a patch to a file PATCHED doesn't list fails" '! sync'
ok "the failure names the unlisted file" 'grep -q "patched but not listed: app/ui/conf.toml" "$T/out"'
printf '# patched files\napp/ui/conf.toml\n' > bluetooth/edition/PATCHED
ok "a path with a space that PATCHED doesn't list fails, named whole" '! sync && grep -q "patched but not listed: app/ui/two words.txt" "$T/out"'

# this repository
cd "$HERE"
: > "$T/out"
missing=0
while IFS= read -r -d '' f; do
  case "$f" in bluetooth/*|.github/*|docs/*|mockups/*|README.md|tools/sync-bluetooth.sh|tools/test-sync-bluetooth.sh) continue ;; esac
  [ -f "$f" ] || continue
  [ -f "bluetooth/$f" ] || { echo "  not in bluetooth/: $f" >> "$T/out"; missing=1; }
done < <(git ls-files -z --cached)
ok "bluetooth/ holds every shared root file" '[ "$missing" -eq 0 ]'
: > "$T/out"
# The store and the Keychain item are the identifier's: the store is
# app_data_dir()/<file> (Tauri puts it under Application Support/<identifier>),
# and the Keychain item's account is that store's full path. So the two
# editions' identifiers differing keeps their stores and keys apart.
ok "the editions' identifiers are app.bombechat.airgapped and app.bombechat.standard" \
  'grep -q "\"identifier\": \"app.bombechat.airgapped\"" app/src-tauri/tauri.conf.json && grep -q "\"identifier\": \"app.bombechat.standard\"" bluetooth/app/src-tauri/tauri.conf.json'
for ed in . bluetooth; do
  ok "$ed: the store lives in the identifier's app data dir, and its Keychain item is that store's" \
    'grep -q "let dir = app.path().app_data_dir()?;" $ed/app/src-tauri/src/lib.rs && grep -q "let store = dir.join(STORE_FILE);" $ed/app/src-tauri/src/lib.rs && grep -q "with_keeper(keychain::system(&store))" $ed/app/src-tauri/src/lib.rs && grep -q "Entry::new(SERVICE, &store_path.to_string_lossy())" $ed/app/src-tauri/src/keychain.rs'
done

echo "$PASSED passed, $FAILED failed, $((PASSED + FAILED)) checks"
[ "$FAILED" -eq 0 ]
