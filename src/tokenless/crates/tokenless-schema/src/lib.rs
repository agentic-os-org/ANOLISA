pub mod schema_compressor;

pub use schema_compressor::SchemaCompressor;

/// Best-effort stderr warning whose own write failure cannot fail the
/// caller. `eprintln!` panics when writing to stderr fails (a full
/// filesystem behind redirected logs, a closed or EPIPE'd descriptor),
/// which turned the stash-rollback diagnostic into a process failure
/// while the compression result itself was fine — the stash layer must
/// stay invisible to it. The write errors are discarded: there is no
/// fallback channel to report a failed warning on, and failing the
/// caller here is exactly the regression to avoid.
pub(crate) fn warn_soft(message: &str) {
    use std::io::Write;
    let mut stderr = std::io::stderr();
    let _ = stderr.write_all(message.as_bytes());
    let _ = stderr.write_all(b"\n");
    let _ = stderr.flush();
}
