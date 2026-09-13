use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

/// A test directory that owns exactly the path it atomically created.
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

impl Drop for OwnedTestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
