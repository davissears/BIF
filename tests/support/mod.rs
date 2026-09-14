use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

/// A test directory that atomically creates a path and never adopts an existing
/// filesystem object.
///
/// Dropping this guard intentionally leaves the directory behind. Portable
/// recursive directory removal APIs accept a pathname, not the originally
/// created directory as a capability. Removing `path` here could therefore
/// delete an unowned replacement if another actor renamed the test directory
/// and recreated its old name. Owned test artifacts are preferable to risking
/// deletion of a real ledger.
pub struct OwnedTestDirectory {
    path: PathBuf,
}

impl OwnedTestDirectory {
    pub fn new() -> Self {
        Self::in_directory(&std::env::temp_dir())
    }

    pub fn in_directory(parent: &Path) -> Self {
        loop {
            let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!(
                "bif-owned-test-directory-{}-{sequence}",
                std::process::id()
            ));

            match fs::create_dir(&path) {
                Ok(()) => return Self { path },
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!(
                    "failed to create owned test directory {}: {error}",
                    path.display()
                ),
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}
