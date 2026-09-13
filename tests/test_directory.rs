mod support;

use std::{
    collections::HashSet,
    fs,
    sync::{Arc, Barrier},
    thread,
};
use support::OwnedTestDirectory;

#[test]
fn owned_test_directories_are_unique_under_parallel_creation() {
    const DIRECTORY_COUNT: usize = 64;

    let parent = Arc::new(OwnedTestDirectory::new());
    let barrier = Arc::new(Barrier::new(DIRECTORY_COUNT));
    let handles: Vec<_> = (0..DIRECTORY_COUNT)
        .map(|_| {
            let parent = Arc::clone(&parent);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                OwnedTestDirectory::in_directory(parent.path())
            })
        })
        .collect();
    let directories: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    let paths: HashSet<_> = directories
        .iter()
        .map(|directory| directory.path())
        .collect();

    assert_eq!(paths.len(), DIRECTORY_COUNT);
    assert!(paths.iter().all(|path| path.is_dir()));
}

#[test]
fn existing_paths_are_never_adopted() {
    const STALE_DIRECTORY_COUNT: u64 = 256;

    let parent = OwnedTestDirectory::new();
    let stale_paths: Vec<_> = (0..STALE_DIRECTORY_COUNT)
        .map(|sequence| {
            parent.path().join(format!(
                "bif-owned-test-directory-{}-{sequence}",
                std::process::id()
            ))
        })
        .collect();
    for path in &stale_paths {
        fs::create_dir(path).unwrap();
        fs::write(path.join("ledger-marker"), "existing").unwrap();
    }

    let owned = OwnedTestDirectory::in_directory(parent.path());

    assert!(!stale_paths.contains(&owned.path().to_owned()));
    assert!(
        stale_paths
            .iter()
            .all(|path| path.join("ledger-marker").is_file())
    );
}

#[test]
fn cleanup_removes_only_the_directory_owned_by_the_guard() {
    let parent = OwnedTestDirectory::new();
    let real_ledger = parent.path().join("real-ledger");
    fs::create_dir(&real_ledger).unwrap();
    fs::write(real_ledger.join("bif.sqlite"), "must survive").unwrap();

    let owned_path = {
        let owned = OwnedTestDirectory::in_directory(parent.path());
        let path = owned.path().to_owned();
        fs::write(path.join("temporary"), "discard").unwrap();
        path
    };

    assert!(!owned_path.exists());
    assert_eq!(
        fs::read_to_string(real_ledger.join("bif.sqlite")).unwrap(),
        "must survive"
    );
}
