#!/usr/bin/env bash
#
# One-shot bootstrap for the Apple side of the workspace.
#
# After cloning the repo, run this script once to:
#   1. Verify the Apple toolchain (xcodebuild + xcodegen) is on PATH.
#   2. Build `DubCore.xcframework` from the Rust core and generate the
#      Swift UniFFI bindings.
#   3. Regenerate `apple/Dub.xcodeproj` from `apple/project.yml`.
#
# After bootstrap, open `apple/Dub.xcodeproj` in Xcode and press Run.
# The window should display "Dub engine OK · v<version>" pulled live
# from the Rust core — the M0.5 smoke screen.
#
# Re-run whenever:
#   * `apple/project.yml` changes (re-regenerates the .xcodeproj)
#   * `crates/dub-ffi/src/lib.rs` changes (re-builds the xcframework
#     and re-generates the Swift bindings)
#
# Idempotent. Safe to run from a clean tree or an already-bootstrapped
# tree; the only persistent state is `apple/DubCore.xcframework`,
# `apple/DubShared/Sources/DubCore/Generated/`, and
# `apple/Dub.xcodeproj`, all of which are gitignored.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${REPO_ROOT}"

# --- Tool checks ---------------------------------------------------------

require_tool() {
    local name="$1"
    local install_hint="$2"
    if ! command -v "${name}" >/dev/null 2>&1; then
        echo "error: '${name}' not found in PATH." >&2
        echo "  install via: ${install_hint}" >&2
        exit 1
    fi
}

require_tool xcodebuild "Xcode 15+ from the Mac App Store"
require_tool xcodegen   "brew install xcodegen"
require_tool cargo      "rustup (https://rustup.rs)"

# --- 1. Build xcframework + Swift bindings -------------------------------

echo "==> Building DubCore.xcframework + Swift bindings"
"${SCRIPT_DIR}/build-xcframework.sh"

# --- 2. Regenerate .xcodeproj from project.yml ---------------------------

echo "==> Running xcodegen"
(cd "${REPO_ROOT}/apple" && xcodegen generate)

# --- 3. Friendly success message ----------------------------------------

cat <<EOF

Bootstrap complete.

Next steps:
  open apple/Dub.xcodeproj
  # Then press Run (⌘R) in Xcode. The window should display
  # "Dub engine OK · v<version>".

If anything goes wrong, re-run this script. It is safe to re-run any
number of times; the only mutated state is gitignored artifacts.
EOF
