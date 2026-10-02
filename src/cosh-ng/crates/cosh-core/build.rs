//! Exports the observability tap symbol so it survives `strip`.
//!
//! `provider::observe::cosh_llm_plaintext_tap` is resolved by name from outside
//! the process, but the release profile sets `strip = true`, which erases
//! `.symtab` and would take the symbol with it. `--export-dynamic` promotes it to
//! `.dynsym`, which strip leaves alone.
//!
//! This lives in a build script rather than `.cargo/config.toml` on purpose: a
//! `RUSTFLAGS` environment variable overrides `target.*.rustflags` from config
//! files entirely, and packaging environments commonly set one, which would drop
//! the flag and silently stop capture. Link args emitted here are appended to the
//! link command instead of competing with `RUSTFLAGS`, and they travel with the
//! crate source into any release tarball.

fn main() {
    // GNU spelling, and therefore Linux-only: Apple's ld64 documents the
    // single-dash `-export_dynamic` and rejects the double-dash form, which
    // would fail every `cosh-core` link on the supported macOS target. Binaries
    // only: the flag is meaningless for the library target.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo::rustc-link-arg-bins=-Wl,--export-dynamic");
    }
}
