#!/usr/bin/env bash
# Proves the airgapped build (--no-default-features) carries no Bluetooth
# or networking crates, and no Bluetooth framework in the built app.
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

# On macOS, also check that the built app does not link CoreBluetooth or
# IOBluetooth: the release binary of the Cargo package bombechat-app, and
# the executable inside the .app bundle, whichever exist (build with
# `cargo tauri build` in app/src-tauri; `--debug` builds land under
# target/debug and are checked too). Only Bluetooth frameworks fail the
# check: WebKit itself links the system networking frameworks.
if [ "$(uname)" = "Darwin" ]; then
  BINARIES=()
  for candidate in \
    target/release/bombechat-app \
    target/release/bundle/macos/Bombechat.app/Contents/MacOS/bombechat-app \
    target/debug/bombechat-app \
    target/debug/bundle/macos/Bombechat.app/Contents/MacOS/bombechat-app; do
    if [ -f "$candidate" ]; then
      BINARIES+=("$candidate")
    fi
  done
  if [ "${#BINARIES[@]}" -eq 0 ]; then
    echo "airgapped: skipping otool binary check, no bombechat-app binary built yet (cargo tauri build in app/src-tauri)"
  fi
  for binary in ${BINARIES[@]+"${BINARIES[@]}"}; do
    BAD_LIBS="$(otool -L "$binary" | grep -E -i 'CoreBluetooth|IOBluetooth' || true)"
    if [ -n "$BAD_LIBS" ]; then
      echo "airgapped: dirty, $binary links a Bluetooth framework:" >&2
      printf '%s\n' "$BAD_LIBS" >&2
      exit 1
    fi
    echo "airgapped: $binary links no Bluetooth framework"
  done
fi

echo "airgapped: clean"
