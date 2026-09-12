use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use bif::{
    config::{ProjectPathMapping, ProjectRemoteMapping},
    domain::ProjectId,
    storage::{self, ProjectRegistrationError, ProjectRepository},
};

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "bif-project-registrations-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn project(value: &str) -> ProjectId {
    ProjectId::new(value).unwrap()
}

fn path_mapping(project_id: &str, path: &Path) -> ProjectPathMapping {
    ProjectPathMapping::new(project(project_id), path).unwrap()
}

#[test]
fn registrations_persist_and_list_in_stable_order() {
    let temp = TempDirectory::new();
    let checkout_a = temp.0.join("a");
    let checkout_b = temp.0.join("b");
    fs::create_dir(&checkout_a).unwrap();
    fs::create_dir(&checkout_b).unwrap();
    let database = temp.0.join("bif.sqlite");

    {
        let mut connection = storage::open(&database).unwrap();
        let mut repository = ProjectRepository::new(&mut connection);
        repository
            .register_path(&path_mapping("second", &checkout_b))
            .unwrap();
        repository
            .register_path(&path_mapping("first", &checkout_a))
            .unwrap();
        repository
            .register_remote(
                &ProjectRemoteMapping::new(project("first"), "git@github.com:Org/Repo.git")
                    .unwrap(),
            )
            .unwrap();
    }

    let mut connection = storage::open(&database).unwrap();
    let repository = ProjectRepository::new(&mut connection);
    let paths = repository.list_paths().unwrap();
    assert_eq!(paths.len(), 2);
    assert_eq!(paths[0].project(), &project("first"));
    assert_eq!(paths[1].project(), &project("second"));
    let remotes = repository.list_remotes().unwrap();
    assert_eq!(remotes.len(), 1);
    assert_eq!(remotes[0].project(), &project("first"));
    assert_eq!(remotes[0].identity().as_str(), "github.com/org/repo");
}

#[test]
fn exact_duplicates_are_idempotent() {
    let temp = TempDirectory::new();
    let checkout = temp.0.join("checkout");
    fs::create_dir(&checkout).unwrap();
    let mut connection = storage::open(temp.0.join("bif.sqlite")).unwrap();
    let mut repository = ProjectRepository::new(&mut connection);
    let path = path_mapping("same", &checkout);
    let remote =
        ProjectRemoteMapping::new(project("same"), "https://github.com/org/repo.git").unwrap();

    repository.register_path(&path).unwrap();
    repository.register_path(&path).unwrap();
    repository.register_remote(&remote).unwrap();
    repository.register_remote(&remote).unwrap();

    assert_eq!(repository.list_paths().unwrap().len(), 1);
    assert_eq!(repository.list_remotes().unwrap().len(), 1);
}

#[test]
fn conflicting_path_and_normalized_remote_return_typed_errors() {
    let temp = TempDirectory::new();
    let checkout = temp.0.join("checkout");
    fs::create_dir(&checkout).unwrap();
    let mut connection = storage::open(temp.0.join("bif.sqlite")).unwrap();
    let mut repository = ProjectRepository::new(&mut connection);
    repository
        .register_path(&path_mapping("first", &checkout))
        .unwrap();
    repository
        .register_remote(
            &ProjectRemoteMapping::new(project("first"), "git@github.com:org/repo.git").unwrap(),
        )
        .unwrap();

    assert!(matches!(
        repository.register_path(&path_mapping("second", &checkout)),
        Err(ProjectRegistrationError::PathConflict { existing, requested, .. })
            if existing == project("first") && requested == project("second")
    ));
    assert!(matches!(
        repository.register_remote(
            &ProjectRemoteMapping::new(project("second"), "https://github.com/org/repo").unwrap()
        ),
        Err(ProjectRegistrationError::RemoteConflict { existing, requested, .. })
            if existing == project("first") && requested == project("second")
    ));
}
