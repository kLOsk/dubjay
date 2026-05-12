//! Thin wrapper around UniFFI's bindgen CLI.
//!
//! Built as a workspace binary so `scripts/build-xcframework.sh` can run
//! `cargo run --bin uniffi-bindgen --features uniffi-cli -- generate ...`
//! without any extra installation step (no `cargo install uniffi-bindgen`
//! required on the developer's machine).
//!
//! See <https://mozilla.github.io/uniffi-rs/latest/tutorial/foreign_language_bindings.html>
//! for the rationale behind this pattern. It boils down to: UniFFI's
//! bindgen needs to read the same `uniffi.toml` and Rust metadata that
//! the *crate* exports, so it's most reliable when shipped from the same
//! crate as the FFI surface.

fn main() {
    uniffi::uniffi_bindgen_main();
}
