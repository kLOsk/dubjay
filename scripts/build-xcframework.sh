#!/usr/bin/env bash
#
# Build DubCore.xcframework from crates/dub-ffi and regenerate the Swift
# UniFFI bindings.
#
# Outputs:
#   apple/DubCore.xcframework/          — multi-arch (aarch64 + x86_64)
#                                         static library bundled as an
#                                         xcframework for the Apple shell.
#   apple/DubShared/Sources/DubCore/    — generated Swift bindings:
#                                         DubCoreFFI.h, DubCoreFFI.modulemap,
#                                         dub_ffi.swift.
#
# Both outputs are gitignored. Re-run this script whenever the Rust
# surface (`#[uniffi::export]` items in `crates/dub-ffi/src/lib.rs`)
# changes.
#
# This script is idempotent — re-running it from a clean tree produces
# byte-identical artifacts modulo timestamps embedded by `ar`.

set -euo pipefail

# --- Locate the repo ------------------------------------------------------

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${REPO_ROOT}"

# --- Configuration --------------------------------------------------------

CRATE_NAME="dub-ffi"
RUST_LIB_NAME="libdub_ffi.a"          # cargo derives this from `name = "dub_ffi"`
CRATE_DIR="crates/${CRATE_NAME}"
APPLE_DIR="apple"
XCFRAMEWORK_PATH="${APPLE_DIR}/DubCore.xcframework"
SWIFT_BINDINGS_DIR="${APPLE_DIR}/DubShared/Sources/DubCore/Generated"

PROFILE="${DUB_RUST_PROFILE:-release}"
TARGETS=(aarch64-apple-darwin x86_64-apple-darwin)

# --- Prerequisites --------------------------------------------------------

require() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "error: '$1' not found in PATH. Install it and re-run." >&2
        exit 1
    fi
}
require cargo
require lipo
require xcodebuild

# Add Rust targets idempotently. `rustup` may not be installed if the
# developer set up a bare toolchain; the `rust-toolchain.toml` already
# pins the targets, but `rustup target add` is the standard idempotent
# command and we shouldn't assume the targets are present.
if command -v rustup >/dev/null 2>&1; then
    for tgt in "${TARGETS[@]}"; do
        rustup target add "${tgt}" >/dev/null 2>&1 || true
    done
fi

# --- Build the static library for each target ----------------------------

for tgt in "${TARGETS[@]}"; do
    echo "==> cargo build --target ${tgt} --profile ${PROFILE} -p ${CRATE_NAME}"
    cargo build \
        --target "${tgt}" \
        --profile "${PROFILE}" \
        -p "${CRATE_NAME}"
done

# --- Fat-binary (universal) build ----------------------------------------

PROFILE_DIR="${PROFILE}"
# `cargo build --profile dev` outputs to `target/<triple>/debug` (not `dev`).
# Map the profile name to the directory cargo actually writes to.
if [[ "${PROFILE}" == "dev" ]]; then
    PROFILE_DIR="debug"
fi

FAT_DIR="target/universal-apple-darwin/${PROFILE_DIR}"
mkdir -p "${FAT_DIR}"
echo "==> lipo -create -output ${FAT_DIR}/${RUST_LIB_NAME}"
lipo -create -output "${FAT_DIR}/${RUST_LIB_NAME}" \
    "target/aarch64-apple-darwin/${PROFILE_DIR}/${RUST_LIB_NAME}" \
    "target/x86_64-apple-darwin/${PROFILE_DIR}/${RUST_LIB_NAME}"

# --- Generate Swift bindings (UniFFI library mode) -----------------------

echo "==> uniffi-bindgen generate (Swift)"
rm -rf "${SWIFT_BINDINGS_DIR}"
mkdir -p "${SWIFT_BINDINGS_DIR}"

# We feed the aarch64 dylib to UniFFI's library-mode bindgen. The dylib
# embeds the UniFFI metadata that the bindgen reads back; either arch
# would do, the metadata is arch-independent.
UNIFFI_LIB="target/aarch64-apple-darwin/${PROFILE_DIR}/libdub_ffi.dylib"
if [[ ! -f "${UNIFFI_LIB}" ]]; then
    echo "==> dylib not found at ${UNIFFI_LIB}; building it explicitly"
    cargo build \
        --target aarch64-apple-darwin \
        --profile "${PROFILE}" \
        -p "${CRATE_NAME}"
fi

cargo run \
    --bin uniffi-bindgen \
    --features uniffi-cli \
    -p "${CRATE_NAME}" \
    -- \
    generate \
    --library "${UNIFFI_LIB}" \
    --language swift \
    --out-dir "${SWIFT_BINDINGS_DIR}"

# UniFFI emits `<crate>.swift`, `<crate>FFI.h`, and `<crate>FFI.modulemap`.
# Rename the modulemap to the conventional `module.modulemap` that
# clang's modulemap search expects when the headers live alongside the
# Swift sources.
if [[ -f "${SWIFT_BINDINGS_DIR}/dub_ffiFFI.modulemap" ]]; then
    mv "${SWIFT_BINDINGS_DIR}/dub_ffiFFI.modulemap" "${SWIFT_BINDINGS_DIR}/module.modulemap"
fi

# --- Assemble the xcframework --------------------------------------------

echo "==> xcodebuild -create-xcframework"
rm -rf "${XCFRAMEWORK_PATH}"

# `-headers` makes the C header from UniFFI part of the xcframework so
# the Swift module's modulemap can resolve `#include "dub_ffiFFI.h"`.
HEADERS_DIR="$(mktemp -d)"
trap 'rm -rf "${HEADERS_DIR}"' EXIT
cp "${SWIFT_BINDINGS_DIR}/dub_ffiFFI.h" "${HEADERS_DIR}/"
# A minimal modulemap inside the xcframework so the C symbols are
# importable from Swift. Module name must match the `#if
# canImport(dub_ffiFFI)` clause in the UniFFI-generated bindings
# (the bindgen derives it from the crate name, not from any flag).
cat > "${HEADERS_DIR}/module.modulemap" <<'EOF'
module dub_ffiFFI {
    umbrella header "dub_ffiFFI.h"
    export *
}
EOF

xcodebuild -create-xcframework \
    -library "${FAT_DIR}/${RUST_LIB_NAME}" -headers "${HEADERS_DIR}" \
    -output "${XCFRAMEWORK_PATH}"

echo "==> Built ${XCFRAMEWORK_PATH}"
echo "==> Swift bindings: ${SWIFT_BINDINGS_DIR}"
