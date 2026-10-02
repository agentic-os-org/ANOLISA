use tokenless_compressors::BuildLogCompressor;

/// Compiler, package-manager, or test-runner output recognized by its domain
/// compressor. Presentation controls alone never establish the content type.
pub(super) fn is_build_log(scan: &str) -> bool {
    BuildLogCompressor::detect(scan)
}
