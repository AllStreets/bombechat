#!/usr/bin/env bash
# Proves the airgapped build (--no-default-features) carries no Bluetooth
# or networking crates, and no Bluetooth framework in the built binary.
#
# Usage:
#   bash tools/check-airgapped.sh
#   CHECK_FEATURES=bluetooth bash tools/check-airgapped.sh   # negative check, must fail
set -euo pipefail

cd "$(dirname "$0")/.."

# Crates that would mean Bluetooth or networking leaked into the airgapped
# build. Matched case-insensitively against `cargo tree` output lines.
BANNED_PATTERN='btleplug|bluer|bluest|corebluetooth|reqwest|hyper|ureq|tauri-plugin-updater|tauri-plugin-http|tokio-tungstenite'

TREE_CMD=(cargo tree --workspace --no-default-features -e normal --prefix none)
if [ -n "${CHECK_FEATURES:-}" ]; then
  TREE_CMD+=(--features "$CHECK_FEATURES")
fi

TREE_OUTPUT="$("${TREE_CMD[@]}")"

OFFENDERS="$(printf '%s\n' "$TREE_OUTPUT" | grep -E -i "$BANNED_PATTERN" || true)"

if [ -n "$OFFENDERS" ]; then
  echo "airgapped: dirty, forbidden crate(s) in dependency tree:" >&2
  printf '%s\n' "$OFFENDERS" >&2
  exit 1
fi

# On macOS, once the app binary exists (Task 11), also check that it does
# not link CoreBluetooth or IOBluetooth. Skip gracefully until then.
if [ "$(uname)" = "Darwin" ]; then
  APP_BINARY="target/release/bombechat-node"
  if [ -f "$APP_BINARY" ]; then
    BAD_LIBS="$(otool -L "$APP_BINARY" | grep -E -i 'CoreBluetooth|IOBluetooth' || true)"
    if [ -n "$BAD_LIBS" ]; then
      echo "airgapped: dirty, binary links a Bluetooth framework:" >&2
      printf '%s\n' "$BAD_LIBS" >&2
      exit 1
    fi
  else
    echo "airgapped: skipping otool binary check, $APP_BINARY does not exist yet (see Task 11)"
  fi
fi

echo "airgapped: clean"
