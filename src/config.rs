//! Configuration, store selection, and project resolution.
//!
//! This outer module may depend inward on domain and application abstractions;
//! neither inner layer depends on configuration.

use crate::domain::{InvalidIdentifier, ProjectId, RequesterId};
use std::{
    collections::HashMap,
    env,
    error::Error,
    fmt, fs,
    io::{self, ErrorKind},
    path::{Path, PathBuf},
};

/// Per-request or command-line configuration values.
///
/// `config` corresponds to `--config`. The other fields are request-level
/// overrides; command parsers can populate them without coupling this module to
/// a particular transport.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ConfigOverrides {
    pub config: Option<PathBuf>,
    pub root: Option<PathBuf>,
    pub requester: Option<String>,
}

/// The resolved configuration and the file selected while loading it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Config {
    pub root: PathBuf,
    pub requester: RequesterId,
    pub source: Option<PathBuf>,
}

/// Canonical filesystem locations for a configured BIF store.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorePaths {
    pub root: PathBuf,
    pub database: PathBuf,
}

/// A project registration whose filesystem path has been canonicalized.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectPathMapping {
    project: ProjectId,
    path: PathBuf,
}

impl ProjectPathMapping {
    /// Creates a mapping for an existing directory.
    pub fn new(project: ProjectId, path: &Path) -> Result<Self, ProjectResolutionError> {
        Ok(Self {
            project,
            path: canonical_directory(path)?,
        })
    }

    pub fn project(&self) -> &ProjectId {
        &self.project
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn from_canonical(project: ProjectId, path: PathBuf) -> Self {
        Self { project, path }
    }
}

/// A validated, in-memory set of registered project paths.
///
/// Registrations for equivalent canonical paths may be repeated only when they
/// name the same project.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProjectPathMappings {
    mappings: Vec<ProjectPathMapping>,
}

impl ProjectPathMappings {
    pub fn new(
        mappings: impl IntoIterator<Item = ProjectPathMapping>,
    ) -> Result<Self, ProjectResolutionError> {
        let mut validated: Vec<ProjectPathMapping> = Vec::new();
        for mapping in mappings {
            if let Some(existing) = validated
                .iter()
                .find(|existing| existing.path == mapping.path)
            {
                if existing.project != mapping.project {
                    return Err(ProjectResolutionError::AmbiguousMapping {
                        path: mapping.path,
                        first: existing.project.clone(),
                        second: mapping.project,
                    });
                }
                continue;
            }
            validated.push(mapping);
        }
        Ok(Self {
            mappings: validated,
        })
    }

    /// Resolves an existing working directory by its longest registered
    /// canonical ancestor. An unrelated directory resolves to `None`.
    pub fn resolve(
        &self,
        working_path: &Path,
    ) -> Result<Option<&ProjectId>, ProjectResolutionError> {
        let working_path = canonical_directory(working_path)?;
        Ok(self
            .mappings
            .iter()
            .filter(|mapping| working_path.starts_with(&mapping.path))
            .max_by_key(|mapping| mapping.path.components().count())
            .map(ProjectPathMapping::project))
    }
}

/// Repository information obtained from Git by an outer adapter.
///
/// Keeping this as parsed data lets callers use a Git library, a subprocess, or
/// fixtures without coupling project resolution to any one Git integration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitMetadata {
    pub repository_root: PathBuf,
    pub remotes: Vec<GitRemote>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitRemote {
    pub name: String,
    pub target: String,
}

/// A transport-independent hosted Git repository identity.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct NormalizedRemoteIdentity(String);

impl NormalizedRemoteIdentity {
    /// Normalizes URL and scp-style Git targets to `host/path`.
    ///
    /// Transport, user information, trailing slashes, and a terminal `.git`
    /// suffix do not contribute to repository identity. Filesystem targets are
    /// intentionally excluded because they are resolved through path mappings.
    pub fn new(target: &str) -> Result<Self, ProjectResolutionError> {
        normalize_remote_identity(target)
            .map(Self)
            .ok_or_else(|| ProjectResolutionError::InvalidRemote(target.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Associates a registered project with a normalized hosted Git remote.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectRemoteMapping {
    project: ProjectId,
    identity: NormalizedRemoteIdentity,
}

impl ProjectRemoteMapping {
    pub fn new(project: ProjectId, target: &str) -> Result<Self, ProjectResolutionError> {
        Ok(Self {
            project,
            identity: NormalizedRemoteIdentity::new(target)?,
        })
    }

    pub fn project(&self) -> &ProjectId {
        &self.project
    }

    pub fn identity(&self) -> &NormalizedRemoteIdentity {
        &self.identity
    }

    pub(crate) fn from_normalized(project: ProjectId, identity: NormalizedRemoteIdentity) -> Self {
        Self { project, identity }
    }
}

/// A validated set of registered remote identities.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProjectRemoteMappings {
    mappings: Vec<ProjectRemoteMapping>,
}

impl ProjectRemoteMappings {
    pub fn new(
        mappings: impl IntoIterator<Item = ProjectRemoteMapping>,
    ) -> Result<Self, ProjectResolutionError> {
        let mut validated: Vec<ProjectRemoteMapping> = Vec::new();
        for mapping in mappings {
            if let Some(existing) = validated
                .iter()
                .find(|existing| existing.identity == mapping.identity)
            {
                if existing.project != mapping.project {
                    return Err(ProjectResolutionError::AmbiguousRemoteMapping {
                        identity: mapping.identity,
                        first: existing.project.clone(),
                        second: mapping.project,
                    });
                }
                continue;
            }
            validated.push(mapping);
        }
        Ok(Self {
            mappings: validated,
        })
    }

    fn resolve(&self, remotes: &[GitRemote]) -> Result<Option<&ProjectId>, ProjectResolutionError> {
        let mut resolved: Option<&ProjectRemoteMapping> = None;
        for remote in remotes {
            let Ok(identity) = NormalizedRemoteIdentity::new(&remote.target) else {
                continue;
            };
            let Some(mapping) = self
                .mappings
                .iter()
                .find(|mapping| mapping.identity == identity)
            else {
                continue;
            };
            if let Some(existing) = resolved {
                if existing.project != mapping.project {
                    return Err(ProjectResolutionError::AmbiguousGitIdentity {
                        first: existing.project.clone(),
                        second: mapping.project.clone(),
                    });
                }
            } else {
                resolved = Some(mapping);
            }
        }
        Ok(resolved.map(|mapping| &mapping.project))
    }
}

/// Resolves Git metadata without invoking Git.
///
/// Registered hosted remotes take precedence. If none match, a Delta-style
/// remote named `local` whose target is a local path is resolved using the same
/// canonical longest-ancestor behavior as a working path.
pub fn resolve_git_project<'a>(
    metadata: &GitMetadata,
    paths: &'a ProjectPathMappings,
    remotes: &'a ProjectRemoteMappings,
) -> Result<Option<&'a ProjectId>, ProjectResolutionError> {
    if let Some(project) = remotes.resolve(&metadata.remotes)? {
        return Ok(Some(project));
    }

    let local_targets: Vec<PathBuf> = metadata
        .remotes
        .iter()
        .filter(|remote| remote.name == "local")
        .filter_map(|remote| local_remote_path(&metadata.repository_root, &remote.target))
        .collect();
    let mut resolved: Option<&ProjectId> = None;
    for target in local_targets {
        if let Some(project) = paths.resolve(&target)? {
            if let Some(existing) = resolved {
                if existing != project {
                    return Err(ProjectResolutionError::AmbiguousGitIdentity {
                        first: existing.clone(),
                        second: project.clone(),
                    });
                }
            } else {
                resolved = Some(project);
            }
        }
    }
    Ok(resolved)
}

/// Resolves a project using the complete project-identity precedence.
///
/// An explicit override wins without consulting the filesystem. Otherwise the
/// working directory is checked against registered paths, followed by
/// registered Git identity. If no registration matches, a Git checkout uses
/// its repository-root directory name; outside Git, the working-directory name
/// is used. Directory-name fallbacks use the normal [`ProjectId`] rules.
pub fn resolve_project(
    explicit: Option<&ProjectId>,
    working_directory: &Path,
    git: Option<&GitMetadata>,
    paths: &ProjectPathMappings,
    remotes: &ProjectRemoteMappings,
) -> Result<ProjectId, ProjectResolutionError> {
    if let Some(project) = explicit {
        return Ok(project.clone());
    }
    if let Some(project) = paths.resolve(working_directory)? {
        return Ok(project.clone());
    }
    if let Some(metadata) = git {
        if let Some(project) = resolve_git_project(metadata, paths, remotes)? {
            return Ok(project.clone());
        }
        return project_from_directory_name(&metadata.repository_root);
    }
    project_from_directory_name(working_directory)
}

fn project_from_directory_name(path: &Path) -> Result<ProjectId, ProjectResolutionError> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    ProjectId::new(name).map_err(|source| ProjectResolutionError::InvalidFallbackName {
        path: path.to_owned(),
        source,
    })
}

/// Errors produced while validating or resolving registered project identity.
#[derive(Debug)]
pub enum ProjectResolutionError {
    InvalidPath {
        path: PathBuf,
        source: io::Error,
    },
    InvalidRemote(String),
    InvalidFallbackName {
        path: PathBuf,
        source: InvalidIdentifier,
    },
    AmbiguousMapping {
        path: PathBuf,
        first: ProjectId,
        second: ProjectId,
    },
    AmbiguousRemoteMapping {
        identity: NormalizedRemoteIdentity,
        first: ProjectId,
        second: ProjectId,
    },
    AmbiguousGitIdentity {
        first: ProjectId,
        second: ProjectId,
    },
}

impl fmt::Display for ProjectResolutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath { path, source } => {
                write!(
                    formatter,
                    "invalid project path {}: {source}",
                    path.display()
                )
            }
            Self::InvalidRemote(target) => {
                write!(formatter, "invalid hosted Git remote {target:?}")
            }
            Self::InvalidFallbackName { path, source } => write!(
                formatter,
                "cannot derive a project from directory {}: {source}",
                path.display()
            ),
            Self::AmbiguousMapping {
                path,
                first,
                second,
            } => write!(
                formatter,
                "project path {} is registered to both {first} and {second}",
                path.display()
            ),
            Self::AmbiguousRemoteMapping {
                identity,
                first,
                second,
            } => write!(
                formatter,
                "Git remote {} is registered to both {first} and {second}",
                identity.as_str()
            ),
            Self::AmbiguousGitIdentity { first, second } => write!(
                formatter,
                "Git metadata resolves to both {first} and {second}"
            ),
        }
    }
}

impl Error for ProjectResolutionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidPath { source, .. } => Some(source),
            Self::InvalidFallbackName { source, .. } => Some(source),
            Self::InvalidRemote(_)
            | Self::AmbiguousMapping { .. }
            | Self::AmbiguousRemoteMapping { .. }
            | Self::AmbiguousGitIdentity { .. } => None,
        }
    }
}

fn normalize_remote_identity(target: &str) -> Option<String> {
    let target = target.trim().trim_end_matches('/');
    let (host, path) = if let Some((scheme, remainder)) = target.split_once("://") {
        if scheme.eq_ignore_ascii_case("file") {
            return None;
        }
        let (authority, path) = remainder.split_once('/')?;
        let host = authority.rsplit('@').next()?.split(':').next()?;
        (host, path)
    } else {
        let (authority, path) = target.split_once(':')?;
        if authority.contains('/')
            || authority.contains('\\')
            || (authority.len() == 1 && authority.as_bytes()[0].is_ascii_alphabetic())
        {
            return None;
        }
        (authority.rsplit('@').next()?, path)
    };
    let lowercase_path = path.trim_matches('/').to_ascii_lowercase();
    let path = lowercase_path
        .strip_suffix(".git")
        .unwrap_or(&lowercase_path)
        .trim_end_matches('/');
    if host.is_empty() || path.is_empty() {
        return None;
    }
    Some(format!("{}/{}", host.to_ascii_lowercase(), path))
}

fn local_remote_path(repository_root: &Path, target: &str) -> Option<PathBuf> {
    let target = target.strip_prefix("file://").unwrap_or(target);
    let path = Path::new(target);
    if path.is_absolute() {
        Some(path.to_owned())
    } else if target.starts_with("./") || target.starts_with("../") {
        Some(repository_root.join(path))
    } else {
        None
    }
}

impl Config {
    /// Resolves this configuration's store locations without creating them.
    pub fn store_paths(&self) -> Result<StorePaths, ConfigError> {
        resolve_store_root(&self.root)
    }
}

/// Stable configuration error categories.
#[derive(Debug)]
pub enum ConfigError {
    NotInitialized,
    InvalidRequester(InvalidIdentifier),
    InvalidFile { path: PathBuf, message: String },
    InvalidRoot { path: PathBuf, source: io::Error },
    Read { path: PathBuf, source: io::Error },
}

impl ConfigError {
    /// Returns the canonical error code used by RPC and CLI adapters.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotInitialized => "not_initialized",
            Self::InvalidRequester(_) | Self::InvalidFile { .. } | Self::InvalidRoot { .. } => {
                "invalid_input"
            }
            Self::Read { .. } => "internal",
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotInitialized => formatter
                .write_str("BIF is not initialized; configure both a store root and requester"),
            Self::InvalidRequester(error) => write!(formatter, "invalid requester: {error}"),
            Self::InvalidFile { path, message } => {
                write!(
                    formatter,
                    "invalid configuration {}: {message}",
                    path.display()
                )
            }
            Self::InvalidRoot { path, source } => {
                write!(formatter, "invalid store root {}: {source}", path.display())
            }
            Self::Read { path, source } => {
                write!(
                    formatter,
                    "could not read configuration {}: {source}",
                    path.display()
                )
            }
        }
    }
}

impl Error for ConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidRequester(error) => Some(error),
            Self::InvalidRoot { source, .. } => Some(source),
            Self::Read { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// An environment source, injectable to avoid changing process-global state.
pub trait Environment {
    fn value(&self, name: &str) -> Option<String>;
}

impl Environment for HashMap<String, String> {
    fn value(&self, name: &str) -> Option<String> {
        self.get(name).cloned()
    }
}

struct ProcessEnvironment;

impl Environment for ProcessEnvironment {
    fn value(&self, name: &str) -> Option<String> {
        env::var(name).ok()
    }
}

/// Loads configuration from the real process environment.
pub fn load(overrides: ConfigOverrides) -> Result<Config, ConfigError> {
    load_with_environment(overrides, &ProcessEnvironment)
}

/// Canonicalizes an existing store root and derives its sole database path.
///
/// The root must already be a directory. The `.bif` directory and database are
/// deliberately not checked or created here, so resolving configuration cannot
/// initialize a store as a side effect.
pub fn resolve_store_root(root: &Path) -> Result<StorePaths, ConfigError> {
    let canonical_root = fs::canonicalize(root).map_err(|source| ConfigError::InvalidRoot {
        path: root.to_owned(),
        source,
    })?;
    let metadata = fs::metadata(&canonical_root).map_err(|source| ConfigError::InvalidRoot {
        path: root.to_owned(),
        source,
    })?;
    if !metadata.is_dir() {
        return Err(ConfigError::InvalidRoot {
            path: root.to_owned(),
            source: io::Error::new(ErrorKind::InvalidInput, "root is not a directory"),
        });
    }

    let database = canonical_root.join(".bif").join("bif.sqlite");
    Ok(StorePaths {
        root: canonical_root,
        database,
    })
}

fn canonical_directory(path: &Path) -> Result<PathBuf, ProjectResolutionError> {
    let canonical =
        fs::canonicalize(path).map_err(|source| ProjectResolutionError::InvalidPath {
            path: path.to_owned(),
            source,
        })?;
    if !fs::metadata(&canonical)
        .map_err(|source| ProjectResolutionError::InvalidPath {
            path: path.to_owned(),
            source,
        })?
        .is_dir()
    {
        return Err(ProjectResolutionError::InvalidPath {
            path: path.to_owned(),
            source: io::Error::new(ErrorKind::InvalidInput, "path is not a directory"),
        });
    }
    Ok(canonical)
}

/// Loads configuration using an injected environment.
///
/// Values are merged field-by-field with request/CLI values taking precedence
/// over environment values, which take precedence over the selected file.
pub fn load_with_environment(
    overrides: ConfigOverrides,
    environment: &impl Environment,
) -> Result<Config, ConfigError> {
    let explicit_file = overrides.config.is_some() || environment.value("BIF_CONFIG").is_some();
    let config_path = overrides
        .config
        .clone()
        .or_else(|| environment.value("BIF_CONFIG").map(PathBuf::from))
        .or_else(|| os_config_path(environment));
    let file_values = match config_path.as_deref() {
        Some(path) => match fs::read_to_string(path) {
            Ok(contents) => parse_file(path, &contents)?,
            Err(error) if error.kind() == ErrorKind::NotFound && !explicit_file => {
                FileValues::default()
            }
            Err(source) => {
                return Err(ConfigError::Read {
                    path: path.to_owned(),
                    source,
                });
            }
        },
        None => FileValues::default(),
    };

    let root = overrides
        .root
        .or_else(|| environment.value("BIF_ROOT").map(PathBuf::from))
        .or(file_values.root);
    let requester = overrides
        .requester
        .or_else(|| environment.value("BIF_REQUESTER"))
        .or(file_values.requester);

    let (Some(root), Some(requester)) = (root, requester) else {
        return Err(ConfigError::NotInitialized);
    };

    Ok(Config {
        root,
        requester: RequesterId::new(requester).map_err(ConfigError::InvalidRequester)?,
        source: config_path.filter(|path| path.exists()),
    })
}

#[derive(Default)]
struct FileValues {
    root: Option<PathBuf>,
    requester: Option<String>,
}

fn parse_file(path: &Path, contents: &str) -> Result<FileValues, ConfigError> {
    let mut values = FileValues::default();
    for (index, original_line) in contents.lines().enumerate() {
        let line = original_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, raw_value) = line
            .split_once('=')
            .ok_or_else(|| ConfigError::InvalidFile {
                path: path.to_owned(),
                message: format!("line {} must be a key/value pair", index + 1),
            })?;
        let value = parse_string(raw_value.trim()).ok_or_else(|| ConfigError::InvalidFile {
            path: path.to_owned(),
            message: format!("line {} must contain a quoted string", index + 1),
        })?;
        match key.trim() {
            "root" if values.root.is_none() => values.root = Some(PathBuf::from(value)),
            "requester" if values.requester.is_none() => values.requester = Some(value),
            "root" | "requester" => {
                return Err(ConfigError::InvalidFile {
                    path: path.to_owned(),
                    message: format!("duplicate key on line {}", index + 1),
                });
            }
            key => {
                return Err(ConfigError::InvalidFile {
                    path: path.to_owned(),
                    message: format!("unknown key {key:?} on line {}", index + 1),
                });
            }
        }
    }
    Ok(values)
}

fn parse_string(value: &str) -> Option<String> {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .map(str::to_owned)
}

fn os_config_path(environment: &impl Environment) -> Option<PathBuf> {
    if cfg!(target_os = "windows") {
        environment
            .value("APPDATA")
            .map(|path| PathBuf::from(path).join("BIF").join("config.toml"))
    } else if cfg!(target_os = "macos") {
        environment.value("HOME").map(|path| {
            PathBuf::from(path)
                .join("Library")
                .join("Application Support")
                .join("BIF")
                .join("config.toml")
        })
    } else {
        environment
            .value("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                environment
                    .value("HOME")
                    .map(|path| PathBuf::from(path).join(".config"))
            })
            .map(|path| path.join("bif").join("config.toml"))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ConfigError, ConfigOverrides, GitMetadata, GitRemote, NormalizedRemoteIdentity,
        ProjectPathMapping, ProjectPathMappings, ProjectRemoteMapping, ProjectRemoteMappings,
        ProjectResolutionError, load_with_environment, resolve_git_project, resolve_project,
        resolve_store_root,
    };
    use crate::domain::ProjectId;
    use crate::test_support::OwnedTestDirectory as TestDirectory;
    use std::{
        collections::HashMap,
        fs,
        path::{Path, PathBuf},
    };

    trait TestDirectoryConfig {
        fn config(&self, name: &str, root: &str, requester: &str) -> PathBuf;
    }

    impl TestDirectoryConfig for TestDirectory {
        fn config(&self, name: &str, root: &str, requester: &str) -> PathBuf {
            let path = self.path().join(name);
            fs::write(
                &path,
                format!("root = \"{root}\"\nrequester = \"{requester}\"\n"),
            )
            .unwrap();
            path
        }
    }

    fn environment(entries: &[(&str, &Path)]) -> HashMap<String, String> {
        entries
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string_lossy().into_owned()))
            .collect()
    }

    fn default_config(directory: &TestDirectory) -> (HashMap<String, String>, PathBuf) {
        if cfg!(target_os = "windows") {
            (
                environment(&[("APPDATA", directory.path())]),
                directory.path().join("BIF").join("config.toml"),
            )
        } else if cfg!(target_os = "macos") {
            (
                environment(&[("HOME", directory.path())]),
                directory
                    .path()
                    .join("Library")
                    .join("Application Support")
                    .join("BIF")
                    .join("config.toml"),
            )
        } else {
            (
                environment(&[("XDG_CONFIG_HOME", directory.path())]),
                directory.path().join("bif").join("config.toml"),
            )
        }
    }

    #[test]
    fn cli_config_path_takes_precedence_over_environment_config_path() {
        let directory = TestDirectory::new();
        let cli = directory.config("cli.toml", "/cli-file", "cli file");
        let env = directory.config("env.toml", "/env-file", "env file");
        let environment = environment(&[("BIF_CONFIG", &env)]);

        let config = load_with_environment(
            ConfigOverrides {
                config: Some(cli.clone()),
                ..ConfigOverrides::default()
            },
            &environment,
        )
        .unwrap();

        assert_eq!(config.root, Path::new("/cli-file"));
        assert_eq!(config.requester.as_str(), "CLI-FILE");
        assert_eq!(config.source.as_deref(), Some(cli.as_path()));
    }

    #[test]
    fn environment_values_take_precedence_over_file_values() {
        let directory = TestDirectory::new();
        let file = directory.config("config.toml", "/file", "file");
        let environment = environment(&[
            ("BIF_CONFIG", &file),
            ("BIF_ROOT", Path::new("/environment")),
            ("BIF_REQUESTER", Path::new("environment")),
        ]);

        let config = load_with_environment(ConfigOverrides::default(), &environment).unwrap();

        assert_eq!(config.root, Path::new("/environment"));
        assert_eq!(config.requester.as_str(), "ENVIRONMENT");
    }

    #[test]
    fn bif_config_takes_precedence_over_selected_os_file() {
        let directory = TestDirectory::new();
        let selected = directory.config("selected.toml", "/selected", "selected");
        let (mut environment, default) = default_config(&directory);
        fs::create_dir_all(default.parent().unwrap()).unwrap();
        fs::write(default, "root = \"/os-file\"\nrequester = \"os file\"\n").unwrap();
        environment.insert(
            "BIF_CONFIG".to_string(),
            selected.to_string_lossy().into_owned(),
        );

        let config = load_with_environment(ConfigOverrides::default(), &environment).unwrap();

        assert_eq!(config.root, Path::new("/selected"));
        assert_eq!(config.requester.as_str(), "SELECTED");
    }

    #[test]
    fn selected_os_file_supplies_configuration() {
        let directory = TestDirectory::new();
        let (environment, default) = default_config(&directory);
        fs::create_dir_all(default.parent().unwrap()).unwrap();
        fs::write(&default, "root = \"/os-file\"\nrequester = \"os file\"\n").unwrap();

        let config = load_with_environment(ConfigOverrides::default(), &environment).unwrap();

        assert_eq!(config.root, Path::new("/os-file"));
        assert_eq!(config.requester.as_str(), "OS-FILE");
        assert_eq!(config.source.as_deref(), Some(default.as_path()));
    }

    #[test]
    fn request_values_take_precedence_over_environment_values() {
        let environment = environment(&[
            ("BIF_ROOT", Path::new("/environment")),
            ("BIF_REQUESTER", Path::new("environment")),
        ]);

        let config = load_with_environment(
            ConfigOverrides {
                root: Some(PathBuf::from("/request")),
                requester: Some("request".to_string()),
                ..ConfigOverrides::default()
            },
            &environment,
        )
        .unwrap();

        assert_eq!(config.root, Path::new("/request"));
        assert_eq!(config.requester.as_str(), "REQUEST");
    }

    #[test]
    fn precedence_is_applied_independently_to_each_field() {
        let directory = TestDirectory::new();
        let file = directory.config("config.toml", "/file", "file");
        let environment = environment(&[
            ("BIF_CONFIG", &file),
            ("BIF_ROOT", Path::new("/environment")),
        ]);

        let config = load_with_environment(ConfigOverrides::default(), &environment).unwrap();

        assert_eq!(config.root, Path::new("/environment"));
        assert_eq!(config.requester.as_str(), "FILE");
    }

    #[test]
    fn absent_configuration_is_not_initialized() {
        let error = load_with_environment(ConfigOverrides::default(), &HashMap::new()).unwrap_err();

        assert!(matches!(error, ConfigError::NotInitialized));
        assert_eq!(error.code(), "not_initialized");
    }

    #[test]
    fn equivalent_relative_and_absolute_roots_resolve_to_one_database_path() {
        let current = std::env::current_dir().unwrap();

        let absolute = resolve_store_root(&current).unwrap();
        let relative = resolve_store_root(Path::new(".")).unwrap();

        assert_eq!(relative, absolute);
        assert!(absolute.root.is_absolute());
        assert_eq!(
            absolute.database,
            absolute.root.join(".bif").join("bif.sqlite")
        );
        assert!(!absolute.database.exists());
        assert!(!absolute.root.join(".bif").exists());
    }

    #[test]
    fn missing_root_is_rejected_without_creating_a_store() {
        let directory = TestDirectory::new();
        let missing = directory.path().join("missing");

        let error = resolve_store_root(&missing).unwrap_err();

        assert!(matches!(error, ConfigError::InvalidRoot { .. }));
        assert_eq!(error.code(), "invalid_input");
        assert!(!missing.exists());
    }

    #[test]
    fn root_must_be_a_directory() {
        let directory = TestDirectory::new();
        let file = directory.path().join("not-a-directory");
        fs::write(&file, "").unwrap();

        let error = resolve_store_root(&file).unwrap_err();

        assert!(matches!(error, ConfigError::InvalidRoot { .. }));
        assert_eq!(error.code(), "invalid_input");
    }

    fn mapping(project: &str, path: &Path) -> ProjectPathMapping {
        ProjectPathMapping::new(ProjectId::new(project).unwrap(), path).unwrap()
    }

    #[test]
    fn nested_mapping_uses_the_longest_canonical_ancestor() {
        let directory = TestDirectory::new();
        let repository = directory.path().join("repository");
        let nested = repository.join("packages").join("api");
        let working = nested.join("src");
        fs::create_dir_all(&working).unwrap();
        let mappings =
            ProjectPathMappings::new([mapping("repository", &repository), mapping("api", &nested)])
                .unwrap();

        let project = mappings.resolve(&working).unwrap().unwrap();

        assert_eq!(project.as_str(), "api");
    }

    #[test]
    fn unrelated_path_has_no_registered_project() {
        let directory = TestDirectory::new();
        let registered = directory.path().join("registered");
        let unrelated = directory.path().join("unrelated");
        fs::create_dir(&registered).unwrap();
        fs::create_dir(&unrelated).unwrap();
        let mappings = ProjectPathMappings::new([mapping("registered", &registered)]).unwrap();

        assert_eq!(mappings.resolve(&unrelated).unwrap(), None);
    }

    #[test]
    fn equivalent_canonical_paths_for_the_same_project_are_one_mapping() {
        let directory = TestDirectory::new();
        let repository = directory.path().join("repository");
        let child = repository.join("child");
        fs::create_dir_all(&child).unwrap();
        let equivalent = repository.join(".");
        let mappings = ProjectPathMappings::new([
            mapping("same project", &repository),
            mapping("same-project", &equivalent),
        ])
        .unwrap();

        assert_eq!(
            mappings.resolve(&child).unwrap().unwrap().as_str(),
            "same-project"
        );
        assert_eq!(mappings.mappings.len(), 1);
    }

    #[test]
    fn equivalent_canonical_paths_for_different_projects_are_ambiguous() {
        let directory = TestDirectory::new();
        let repository = directory.path().join("repository");
        fs::create_dir(&repository).unwrap();
        let equivalent = repository.join(".");

        let error = ProjectPathMappings::new([
            mapping("first", &repository),
            mapping("second", &equivalent),
        ])
        .unwrap_err();

        assert!(matches!(
            error,
            ProjectResolutionError::AmbiguousMapping { .. }
        ));
    }

    fn remote(name: &str, target: impl Into<String>) -> GitRemote {
        GitRemote {
            name: name.to_owned(),
            target: target.into(),
        }
    }

    fn remote_mapping(project: &str, target: &str) -> ProjectRemoteMapping {
        ProjectRemoteMapping::new(ProjectId::new(project).unwrap(), target).unwrap()
    }

    #[test]
    fn normalizes_equivalent_hosted_remote_syntax() {
        let https = NormalizedRemoteIdentity::new("https://GitHub.com/Acme/Widgets.git/").unwrap();
        let ssh = NormalizedRemoteIdentity::new("git@github.com:acme/widgets").unwrap();
        let ssh_url =
            NormalizedRemoteIdentity::new("ssh://git@GITHUB.COM/acme/widgets.git").unwrap();

        assert_eq!(https, ssh);
        assert_eq!(https, ssh_url);
        assert_eq!(https.as_str(), "github.com/acme/widgets");
    }

    #[test]
    fn two_delta_checkouts_with_the_same_local_target_resolve_identically() {
        let directory = TestDirectory::new();
        let registered = directory.path().join("primary-checkout");
        let delta_one = directory.path().join("delta-one");
        let delta_two = directory.path().join("elsewhere").join("delta-two");
        fs::create_dir_all(&registered).unwrap();
        fs::create_dir_all(&delta_one).unwrap();
        fs::create_dir_all(&delta_two).unwrap();
        let paths = ProjectPathMappings::new([mapping("widgets", &registered)]).unwrap();
        let remotes = ProjectRemoteMappings::default();
        let target = registered.to_string_lossy().into_owned();
        let first = GitMetadata {
            repository_root: delta_one,
            remotes: vec![remote("local", target.clone())],
        };
        let second = GitMetadata {
            repository_root: delta_two,
            remotes: vec![remote("local", target)],
        };

        assert_eq!(
            resolve_git_project(&first, &paths, &remotes)
                .unwrap()
                .unwrap()
                .as_str(),
            "widgets"
        );
        assert_eq!(
            resolve_git_project(&second, &paths, &remotes)
                .unwrap()
                .unwrap()
                .as_str(),
            "widgets"
        );
    }

    #[test]
    fn hosted_remote_match_takes_precedence_over_delta_local_remote() {
        let directory = TestDirectory::new();
        let local = directory.path().join("local");
        fs::create_dir(&local).unwrap();
        let paths = ProjectPathMappings::new([mapping("local-project", &local)]).unwrap();
        let remotes = ProjectRemoteMappings::new([remote_mapping(
            "hosted-project",
            "https://github.com/acme/widgets.git",
        )])
        .unwrap();
        let metadata = GitMetadata {
            repository_root: directory.path().to_owned(),
            remotes: vec![
                remote("origin", "git@github.com:ACME/WIDGETS"),
                remote("local", local.to_string_lossy()),
            ],
        };

        assert_eq!(
            resolve_git_project(&metadata, &paths, &remotes)
                .unwrap()
                .unwrap()
                .as_str(),
            "hosted-project"
        );
    }

    #[test]
    fn conflicting_remote_registration_is_rejected() {
        let error = ProjectRemoteMappings::new([
            remote_mapping("first", "https://github.com/acme/widgets.git"),
            remote_mapping("second", "git@github.com:acme/widgets"),
        ])
        .unwrap_err();

        assert!(matches!(
            error,
            ProjectResolutionError::AmbiguousRemoteMapping { .. }
        ));
    }

    #[test]
    fn conflicting_matched_git_remotes_are_ambiguous() {
        let remotes = ProjectRemoteMappings::new([
            remote_mapping("first", "https://github.com/acme/first"),
            remote_mapping("second", "https://github.com/acme/second"),
        ])
        .unwrap();
        let metadata = GitMetadata {
            repository_root: PathBuf::from("/unused"),
            remotes: vec![
                remote("origin", "git@github.com:acme/first.git"),
                remote("upstream", "git@github.com:acme/second.git"),
            ],
        };

        let error =
            resolve_git_project(&metadata, &ProjectPathMappings::default(), &remotes).unwrap_err();

        assert!(matches!(
            error,
            ProjectResolutionError::AmbiguousGitIdentity { .. }
        ));
    }

    #[test]
    fn invalid_registered_remote_is_rejected_and_unmatched_metadata_returns_none() {
        assert!(matches!(
            NormalizedRemoteIdentity::new("/a/local/path"),
            Err(ProjectResolutionError::InvalidRemote(_))
        ));
        let metadata = GitMetadata {
            repository_root: PathBuf::from("/unused"),
            remotes: vec![remote("origin", "https://github.com/unknown/project")],
        };

        assert_eq!(
            resolve_git_project(
                &metadata,
                &ProjectPathMappings::default(),
                &ProjectRemoteMappings::default()
            )
            .unwrap(),
            None
        );
    }

    fn metadata(root: PathBuf, remotes: Vec<GitRemote>) -> GitMetadata {
        GitMetadata {
            repository_root: root,
            remotes,
        }
    }

    #[test]
    fn full_resolution_uses_each_tier_in_documented_order() {
        let directory = TestDirectory::new();
        let registered = directory.path().join("registered");
        let working = registered.join("src");
        let local = directory.path().join("local");
        fs::create_dir_all(&working).unwrap();
        fs::create_dir(&local).unwrap();
        let paths = ProjectPathMappings::new([
            mapping("path-tier", &registered),
            mapping("local-tier", &local),
        ])
        .unwrap();
        let remotes = ProjectRemoteMappings::new([remote_mapping(
            "remote-tier",
            "https://example.com/acme/project",
        )])
        .unwrap();
        let git = metadata(
            directory.path().join("Fallback Repository"),
            vec![
                remote("origin", "git@example.com:acme/project.git"),
                remote("local", local.to_string_lossy()),
            ],
        );
        let explicit = ProjectId::new("explicit tier").unwrap();

        assert_eq!(
            resolve_project(
                Some(&explicit),
                Path::new("/missing"),
                Some(&git),
                &paths,
                &remotes
            )
            .unwrap()
            .as_str(),
            "explicit-tier"
        );
        assert_eq!(
            resolve_project(None, &working, Some(&git), &paths, &remotes)
                .unwrap()
                .as_str(),
            "path-tier"
        );

        let unrelated = directory.path().join("unrelated");
        fs::create_dir(&unrelated).unwrap();
        assert_eq!(
            resolve_project(None, &unrelated, Some(&git), &paths, &remotes)
                .unwrap()
                .as_str(),
            "remote-tier"
        );

        let no_hosted_match = metadata(
            directory.path().join("Fallback Repository"),
            vec![remote("local", local.to_string_lossy())],
        );
        assert_eq!(
            resolve_project(None, &unrelated, Some(&no_hosted_match), &paths, &remotes)
                .unwrap()
                .as_str(),
            "local-tier"
        );
    }

    #[test]
    fn recreated_checkouts_and_changed_working_directories_use_repository_name() {
        let directory = TestDirectory::new();
        let first = directory
            .path()
            .join("first")
            .join("My Recreated_Project.git");
        let second = directory
            .path()
            .join("second")
            .join("My Recreated_Project.git");
        let first_working = first.join("packages").join("one");
        let second_working = second.join("somewhere").join("else");
        fs::create_dir_all(&first_working).unwrap();
        fs::create_dir_all(&second_working).unwrap();

        let first_project = resolve_project(
            None,
            &first_working,
            Some(&metadata(first, vec![])),
            &ProjectPathMappings::default(),
            &ProjectRemoteMappings::default(),
        )
        .unwrap();
        let second_project = resolve_project(
            None,
            &second_working,
            Some(&metadata(second, vec![])),
            &ProjectPathMappings::default(),
            &ProjectRemoteMappings::default(),
        )
        .unwrap();

        assert_eq!(first_project.as_str(), "my-recreated-project-git");
        assert_eq!(second_project, first_project);
    }

    #[test]
    fn outside_git_uses_normalized_current_directory_name() {
        let directory = TestDirectory::new();
        let working = directory.path().join("Current DIRECTORY_name");
        fs::create_dir(&working).unwrap();

        let project = resolve_project(
            None,
            &working,
            None,
            &ProjectPathMappings::default(),
            &ProjectRemoteMappings::default(),
        )
        .unwrap();

        assert_eq!(project.as_str(), "current-directory-name");
    }

    #[test]
    fn fallback_rejects_names_with_empty_normalization() {
        let directory = TestDirectory::new();
        let working = directory.path().join("___");
        fs::create_dir(&working).unwrap();

        let error = resolve_project(
            None,
            &working,
            None,
            &ProjectPathMappings::default(),
            &ProjectRemoteMappings::default(),
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ProjectResolutionError::InvalidFallbackName { .. }
        ));
    }

    #[test]
    fn git_ambiguity_is_not_hidden_by_repository_fallback() {
        let directory = TestDirectory::new();
        let working = directory.path().join("working");
        fs::create_dir(&working).unwrap();
        let remotes = ProjectRemoteMappings::new([
            remote_mapping("first", "https://example.com/acme/first"),
            remote_mapping("second", "https://example.com/acme/second"),
        ])
        .unwrap();
        let git = metadata(
            directory.path().join("fallback"),
            vec![
                remote("one", "git@example.com:acme/first"),
                remote("two", "git@example.com:acme/second"),
            ],
        );

        let error = resolve_project(
            None,
            &working,
            Some(&git),
            &ProjectPathMappings::default(),
            &remotes,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ProjectResolutionError::AmbiguousGitIdentity { .. }
        ));
    }
}
