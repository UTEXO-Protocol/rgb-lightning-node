//! Select the active Android system CA store without importing stale roots from another store.
use std::path::Path;
pub(super) fn select_store<'a>(candidates: &[&'a Path]) -> Option<&'a Path> {
    candidates.iter().copied().find(|path| {
        std::fs::read_dir(path)
            .map(|mut entries| entries.next().is_some())
            .unwrap_or(false)
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prefer_nonempty_updatable_store_and_fall_back_on_older_android() {
        let root = std::env::temp_dir().join(format!(
            "bfa-ca-store-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let apex = root.join("apex");
        let system = root.join("system");
        std::fs::create_dir_all(&system).unwrap();
        let dirs = [apex.as_path(), system.as_path()];
        assert_eq!(select_store(&dirs), None);
        std::fs::write(system.join("root.0"), b"system certificate").unwrap();
        assert_eq!(select_store(&dirs), Some(system.as_path()));
        std::fs::create_dir(&apex).unwrap();
        assert_eq!(select_store(&dirs), Some(system.as_path()));
        std::fs::write(apex.join("root.0"), b"updated certificate").unwrap();
        assert_eq!(select_store(&dirs), Some(apex.as_path()));
        std::fs::remove_dir_all(root).unwrap();
    }
}
