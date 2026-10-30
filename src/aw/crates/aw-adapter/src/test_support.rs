//! Private launch-file ownership for native integration unit tests.

pub(crate) struct Artifacts(pub(crate) std::path::PathBuf);

impl crate::LaunchFiles for Artifacts {
    fn directory(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for Artifacts {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
