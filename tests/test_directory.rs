mod support;

use std::{
    collections::HashSet,
    fs,
    sync::{Arc, Barrier, mpsc},
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
fn drop_intentionally_leaves_owned_directory_orphaned() {
    let parent = OwnedTestDirectory::new();

    let owned_path = {
        let owned = OwnedTestDirectory::in_directory(parent.path());
        let path = owned.path().to_owned();
        fs::write(path.join("temporary"), "discard").unwrap();
        path
    };

    assert_eq!(
        fs::read_to_string(owned_path.join("temporary")).unwrap(),
        "discard"
    );
}

#[test]
fn drop_never_deletes_a_replacement_directory() {
    let parent = OwnedTestDirectory::new();
    let owned = OwnedTestDirectory::in_directory(parent.path());
    let original_path = owned.path().to_owned();
    let renamed_path = parent.path().join("renamed-owned-directory");
    fs::write(original_path.join("owned-marker"), "owned orphan").unwrap();

    let (replacement_ready_tx, replacement_ready_rx) = mpsc::sync_channel(0);
    let actor_path = original_path.clone();
    let actor_renamed_path = renamed_path.clone();
    let actor = thread::spawn(move || {
        fs::rename(&actor_path, &actor_renamed_path).unwrap();
        fs::create_dir(&actor_path).unwrap();
        fs::write(actor_path.join("bif.sqlite"), "must survive").unwrap();
        replacement_ready_tx.send(()).unwrap();
    });

    replacement_ready_rx.recv().unwrap();
    drop(owned);
    actor.join().unwrap();

    assert_eq!(
        fs::read_to_string(original_path.join("bif.sqlite")).unwrap(),
        "must survive"
    );
    assert_eq!(
        fs::read_to_string(renamed_path.join("owned-marker")).unwrap(),
        "owned orphan"
    );
}
