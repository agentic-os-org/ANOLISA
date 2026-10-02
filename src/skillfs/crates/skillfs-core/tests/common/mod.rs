use std::path::PathBuf;

/// Load a test fixture file by name (without extension).
pub fn load_fixture(name: &str) -> String {
    let fixture_path = fixture_path(name);
    std::fs::read_to_string(&fixture_path)
        .unwrap_or_else(|e| panic!("failed to load fixture {}: {e}", fixture_path.display()))
}

/// Get the path to a fixture file.
pub fn fixture_path(name: &str) -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("tests");
    path.push("fixtures");
    path.push(name);
    path
}
