//! Helpers shared by the unit tests.

use std::path::{Path, PathBuf};

/// Every scenario file in `scenarios/`, subfolders included, sorted.
pub(crate) fn bundled_scenarios() -> Vec<PathBuf> {
    fn walk(dir: &Path, found: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, found);
            } else if path.extension().is_some_and(|ext| ext == "toml") {
                found.push(path);
            }
        }
    }
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../scenarios");
    let mut found = Vec::new();
    walk(&dir, &mut found);
    found.sort();
    assert!(!found.is_empty(), "no scenarios found in {}", dir.display());
    found
}
