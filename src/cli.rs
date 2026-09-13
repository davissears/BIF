//! Human command parsing and concise rendering.
//!
//! This delivery module may depend inward on application and domain code;
//! neither inner layer depends on CLI concerns.

use crate::{
    application::{
        Actor, ActorKind, AuthorizationRequest, CaptureIdentity, CaptureInput, CaptureRequest,
        Clock, Command as ApplicationCommand, Execution, IdentityGenerator, ObservedExecution,
    },
    config::{
        self, ConfigOverrides, GitMetadata, GitRemote, ProjectPathMapping, ProjectPathMappings,
        ProjectRemoteMappings, resolve_project,
    },
    domain::{
        ItemContent, MessageId, ProjectId, Provenance, RepositoryReference, RevisionReference,
        SourceHost, SourceUrl, ThreadId, Timestamp,
    },
    storage::{self, CaptureRepository, ProjectRepository},
};
use std::{
    env,
    ffi::OsString,
    fmt, fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

const USAGE: &str = "Usage:
  bif init --root PATH --requester NAME [--config PATH]
  bif doctor [--config PATH] [--root PATH] [--requester NAME]
  bif project register SLUG --path PATH [--config PATH]
  bif project list [--config PATH]
  bif capture TITLE --idempotency-key KEY [--description TEXT] [--acceptance TEXT]...
      [--project SLUG] [--requester NAME] [--config PATH] [--root PATH]
      [--source-host delta|codex|local] [--thread-id ID] [--message-id ID]
      [--url URL] [--repository-reference REF] [--revision-reference REF]
      [--context-excerpt TEXT]
  bif get ITEM_ID [--json] [--config PATH] [--root PATH] [--requester NAME]
  bif list [VIEW] [FILTERS] [--limit N] [--offset N] [--json]
  bif next [FILTERS] [--limit N] [--offset N] [--json]
  bif history ITEM_ID [--json] [--config PATH] [--root PATH] [--requester NAME]
  bif triage ITEM_ID --expected-revision N --idempotency-key KEY
      [--action ACTION] [--reason TEXT] [--priority P0|P1|P2|P3|P4|clear]
      [--assignee NAME|clear] [--note TEXT] [--config PATH] [--root PATH]
      [--requester NAME]
  bif approve|start|resume|finish ITEM_ID --expected-revision N --idempotency-key KEY
  bif reject|block ITEM_ID REASON --expected-revision N --idempotency-key KEY
  bif prioritize ITEM_ID PRIORITY --expected-revision N --idempotency-key KEY
  bif assign ITEM_ID ASSIGNEE --expected-revision N --idempotency-key KEY";

/// Runs a command and returns a stable process exit status.
pub fn run<I, S>(arguments: I, stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let arguments = match arguments
        .into_iter()
        .map(|value| {
            value
                .into()
                .into_string()
                .map_err(|_| CliError::Usage("arguments must be valid UTF-8".into()))
        })
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(arguments) => arguments,
        Err(error) => {
            let _ = writeln!(stderr, "error: {error}");
            let _ = writeln!(stderr, "{USAGE}");
            return 2;
        }
    };
    if let Some(result) = crate::cli_read::run(&arguments) {
        return match result {
            Ok(output) => match stdout.write_all(output.as_bytes()) {
                Ok(()) => 0,
                Err(error) => {
                    let _ = writeln!(stderr, "error: could not write output: {error}");
                    1
                }
            },
            Err(error) => {
                let _ = writeln!(stderr, "error: {error}");
                if error.usage {
                    let _ = writeln!(stderr, "{USAGE}");
                }
                error.exit_code
            }
        };
    }
    match parse(arguments).and_then(execute) {
        Ok(output) => {
            if let Err(error) = stdout.write_all(output.as_bytes()) {
                let _ = writeln!(stderr, "error: could not write output: {error}");
                return 1;
            }
            0
        }
        Err(error) => {
            let _ = writeln!(stderr, "error: {error}");
            if matches!(error, CliError::Usage(_)) {
                let _ = writeln!(stderr, "{USAGE}");
                2
            } else {
                1
            }
        }
    }
}

#[derive(Debug)]
enum Command {
    Init {
        root: PathBuf,
        requester: String,
        config: Option<PathBuf>,
    },
    Doctor(ConfigOverrides),
    Register {
        project: String,
        path: PathBuf,
        config: Option<PathBuf>,
    },
    List {
        config: Option<PathBuf>,
    },
    Capture(CaptureOptions),
    Mutation(crate::cli_mutation::MutationOptions),
}

#[derive(Debug)]
struct CaptureOptions {
    title: String,
    description: Option<String>,
    acceptance_criteria: Vec<String>,
    idempotency_key: String,
    project: Option<String>,
    config: ConfigOverrides,
    source_host: SourceHost,
    thread_id: Option<String>,
    message_id: Option<String>,
    url: Option<String>,
    repository_reference: Option<String>,
    revision_reference: Option<String>,
    context_excerpt: Option<String>,
}

fn parse<I, S>(arguments: I) -> Result<Command, CliError>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let values = arguments
        .into_iter()
        .map(|value| {
            value
                .into()
                .into_string()
                .map_err(|_| CliError::Usage("arguments must be valid UTF-8".into()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let Some(command) = values.first().map(String::as_str) else {
        return Err(CliError::Usage("a command is required".into()));
    };

    match command {
        "init" => {
            let options = Options::parse(&values[1..], &["root", "requester", "config"])?;
            Ok(Command::Init {
                root: PathBuf::from(options.required("root")?),
                requester: options.required("requester")?.to_owned(),
                config: options.get("config").map(PathBuf::from),
            })
        }
        "doctor" => {
            let options = Options::parse(&values[1..], &["config", "root", "requester"])?;
            Ok(Command::Doctor(ConfigOverrides {
                config: options.get("config").map(PathBuf::from),
                root: options.get("root").map(PathBuf::from),
                requester: options.get("requester").map(str::to_owned),
            }))
        }
        "project" if values.get(1).map(String::as_str) == Some("register") => {
            let project = values
                .get(2)
                .filter(|value| !value.starts_with("--"))
                .ok_or_else(|| CliError::Usage("project register requires SLUG".into()))?;
            let options = Options::parse(&values[3..], &["path", "config"])?;
            Ok(Command::Register {
                project: project.clone(),
                path: PathBuf::from(options.required("path")?),
                config: options.get("config").map(PathBuf::from),
            })
        }
        "project" if values.get(1).map(String::as_str) == Some("list") => {
            let options = Options::parse(&values[2..], &["config"])?;
            Ok(Command::List {
                config: options.get("config").map(PathBuf::from),
            })
        }
        "capture" => parse_capture(&values[1..]).map(Command::Capture),
        mutation if crate::cli_mutation::is_command(mutation) => {
            crate::cli_mutation::parse(mutation, &values[1..]).map(Command::Mutation)
        }
        "project" => Err(CliError::Usage(
            "project requires either register or list".into(),
        )),
        other => Err(CliError::Usage(format!("unknown command {other:?}"))),
    }
}

fn parse_capture(values: &[String]) -> Result<CaptureOptions, CliError> {
    let title = values
        .first()
        .filter(|value| !value.starts_with("--"))
        .ok_or_else(|| CliError::Usage("capture requires TITLE".into()))?
        .clone();
    let options = Options::parse_repeated(
        &values[1..],
        &[
            "description",
            "acceptance",
            "idempotency-key",
            "project",
            "requester",
            "config",
            "root",
            "source-host",
            "thread-id",
            "message-id",
            "url",
            "repository-reference",
            "revision-reference",
            "context-excerpt",
        ],
        &["acceptance"],
    )?;
    let source_host = match options.get("source-host").unwrap_or("local") {
        "delta" => SourceHost::Delta,
        "codex" => SourceHost::Codex,
        "local" => SourceHost::Local,
        value => {
            return Err(CliError::Usage(format!(
                "invalid --source-host {value:?}; expected delta, codex, or local"
            )));
        }
    };
    let idempotency_key = options.required("idempotency-key")?.to_owned();
    if idempotency_key.is_empty() {
        return Err(CliError::Usage(
            "--idempotency-key must not be empty".into(),
        ));
    }
    Ok(CaptureOptions {
        title,
        description: options.get("description").map(str::to_owned),
        acceptance_criteria: options.all("acceptance").map(str::to_owned).collect(),
        idempotency_key,
        project: options.get("project").map(str::to_owned),
        config: ConfigOverrides {
            config: options.get("config").map(PathBuf::from),
            root: options.get("root").map(PathBuf::from),
            requester: options.get("requester").map(str::to_owned),
        },
        source_host,
        thread_id: options.get("thread-id").map(str::to_owned),
        message_id: options.get("message-id").map(str::to_owned),
        url: options.get("url").map(str::to_owned),
        repository_reference: options.get("repository-reference").map(str::to_owned),
        revision_reference: options.get("revision-reference").map(str::to_owned),
        context_excerpt: options.get("context-excerpt").map(str::to_owned),
    })
}

struct Options<'a>(Vec<(&'a str, &'a str)>);

impl<'a> Options<'a> {
    fn parse(values: &'a [String], allowed: &[&str]) -> Result<Self, CliError> {
        Self::parse_repeated(values, allowed, &[])
    }

    fn parse_repeated(
        values: &'a [String],
        allowed: &[&str],
        repeatable: &[&str],
    ) -> Result<Self, CliError> {
        let mut parsed = Vec::new();
        let mut index = 0;
        while index < values.len() {
            let option = values[index].strip_prefix("--").ok_or_else(|| {
                CliError::Usage(format!("unexpected argument {:?}", values[index]))
            })?;
            if !allowed.contains(&option) {
                return Err(CliError::Usage(format!("unknown option --{option}")));
            }
            if !repeatable.contains(&option) && parsed.iter().any(|(name, _)| *name == option) {
                return Err(CliError::Usage(format!("duplicate option --{option}")));
            }
            let value = values
                .get(index + 1)
                .filter(|value| !value.starts_with("--"))
                .ok_or_else(|| CliError::Usage(format!("--{option} requires a value")))?;
            parsed.push((option, value.as_str()));
            index += 2;
        }
        Ok(Self(parsed))
    }

    fn get(&self, name: &str) -> Option<&'a str> {
        self.0
            .iter()
            .find_map(|(candidate, value)| (*candidate == name).then_some(*value))
    }

    fn required(&self, name: &str) -> Result<&'a str, CliError> {
        self.get(name)
            .ok_or_else(|| CliError::Usage(format!("missing required option --{name}")))
    }

    fn all(&self, name: &'a str) -> impl Iterator<Item = &'a str> + '_ {
        self.0
            .iter()
            .filter_map(move |(candidate, value)| (*candidate == name).then_some(*value))
    }
}

fn execute(command: Command) -> Result<String, CliError> {
    match command {
        Command::Init {
            root,
            requester,
            config,
        } => initialize(&root, &requester, config.as_deref()),
        Command::Doctor(overrides) => doctor(overrides),
        Command::Register {
            project,
            path,
            config,
        } => register(&project, &path, config),
        Command::List { config } => list(config),
        Command::Capture(options) => capture(options),
        Command::Mutation(options) => crate::cli_mutation::execute(options),
    }
}

fn capture(options: CaptureOptions) -> Result<String, CliError> {
    let config = config::load(options.config)?;
    let paths = config.store_paths()?;
    let mut connection = storage::open(&paths.database)?;
    let repository = ProjectRepository::new(&mut connection);
    let path_mappings = ProjectPathMappings::new(repository.list_paths()?)?;
    let remote_mappings = ProjectRemoteMappings::new(repository.list_remotes()?)?;
    let working_directory = env::current_dir()?;
    let git = git_metadata(&working_directory);
    let explicit_project = options.project.as_deref().map(ProjectId::new).transpose()?;
    let project = resolve_project(
        explicit_project.as_ref(),
        &working_directory,
        git.as_ref(),
        &path_mappings,
        &remote_mappings,
    )?;
    let provenance = Provenance::new(
        Some(options.source_host),
        options.thread_id.map(ThreadId::new),
        options.message_id.map(MessageId::new),
        options.url.map(SourceUrl::new),
        options.repository_reference.map(RepositoryReference::new),
        options.revision_reference.map(RevisionReference::new),
        options.context_excerpt,
    );
    let input = CaptureInput {
        requester: config.requester.clone(),
        project,
        content: ItemContent::new(
            options.title,
            options.description,
            options.acceptance_criteria,
        )?,
        provenance,
    };
    let actor_id = config.requester.to_string();
    let authorization = AuthorizationRequest {
        actor: Actor {
            kind: ActorKind::Human,
            id: &actor_id,
            surface: "cli",
            host: "local",
        },
        execution: Execution::Direct {
            surface: "cli",
            host: "local",
        },
        observed_execution: ObservedExecution::Direct,
        command: ApplicationCommand::Capture,
        human_authorization: None,
    };
    let result = crate::application::capture(
        &mut CaptureRepository::new(&mut connection),
        &mut SystemClock,
        &mut SystemIdentities,
        &authorization,
        CaptureRequest {
            idempotency_key: options.idempotency_key,
            input,
        },
    )?;
    let source = match result.item.provenance().source_host() {
        Some(SourceHost::Delta) => "delta",
        Some(SourceHost::Codex) => "codex",
        Some(SourceHost::Local) => "local",
        None => "unavailable",
    };
    Ok(format!(
        "Captured {}\ntitle: {}\nstatus: proposed\nreplayed: {}\nsource: {source}\n",
        result.item.id(),
        result.item.content().title(),
        result.replayed
    ))
}

struct SystemClock;

impl Clock for SystemClock {
    fn now(&mut self) -> Timestamp {
        Timestamp::new(rfc3339_now())
    }
}

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

struct SystemIdentities;

impl IdentityGenerator for SystemIdentities {
    fn capture_identity(&mut self) -> CaptureIdentity {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let serial = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        CaptureIdentity {
            operation_id: format!("cli-{nonce:032x}-{serial:016x}"),
            event_id: format!("cli-event-{nonce:032x}-{serial:016x}"),
        }
    }
}

pub(crate) fn rfc3339_now() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let days = seconds / 86_400;
    let day_seconds = seconds % 86_400;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        day_seconds / 3_600,
        day_seconds / 60 % 60,
        day_seconds % 60
    )
}

fn initialize(
    root: &Path,
    requester: &str,
    explicit_config: Option<&Path>,
) -> Result<String, CliError> {
    let paths = config::resolve_store_root(root)?;
    let requester = crate::domain::RequesterId::new(requester)?;
    let config_path = selected_config_path(explicit_config)?;
    let contents = format!(
        "root = \"{}\"\nrequester = \"{}\"\n",
        config_string(&paths.root)?,
        requester
    );
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent)?;
    }
    if fs::read_to_string(&config_path).ok().as_deref() != Some(&contents) {
        fs::write(&config_path, contents)?;
    }
    fs::create_dir_all(paths.database.parent().expect("database has a parent"))?;
    storage::open(&paths.database)?;
    Ok(format!(
        "Initialized BIF\nconfig: {}\nstore: {}\nrequester: {}\n",
        config_path.display(),
        paths.database.display(),
        requester
    ))
}

fn register(project: &str, path: &Path, config_path: Option<PathBuf>) -> Result<String, CliError> {
    let config = config::load(ConfigOverrides {
        config: config_path,
        ..ConfigOverrides::default()
    })?;
    let paths = config.store_paths()?;
    let project = ProjectId::new(project)?;
    let mapping = ProjectPathMapping::new(project, path)?;
    let mut connection = storage::open(&paths.database)?;
    ProjectRepository::new(&mut connection).register_path(&mapping)?;
    Ok(format!(
        "Registered {} {}\n",
        mapping.project(),
        mapping.path().display()
    ))
}

fn list(config_path: Option<PathBuf>) -> Result<String, CliError> {
    let config = config::load(ConfigOverrides {
        config: config_path,
        ..ConfigOverrides::default()
    })?;
    let paths = config.store_paths()?;
    let mut connection = storage::open(&paths.database)?;
    let repository = ProjectRepository::new(&mut connection);
    let mut output = String::new();
    for mapping in repository.list_paths()? {
        output.push_str(&format!(
            "{}\tpath\t{}\n",
            mapping.project(),
            mapping.path().display()
        ));
    }
    for mapping in repository.list_remotes()? {
        output.push_str(&format!(
            "{}\tremote\t{}\n",
            mapping.project(),
            mapping.identity().as_str()
        ));
    }
    if output.is_empty() {
        output.push_str("No registered projects.\n");
    }
    Ok(output)
}

fn doctor(overrides: ConfigOverrides) -> Result<String, CliError> {
    let config = config::load(overrides)?;
    let paths = config.store_paths()?;
    let mut connection = storage::open(&paths.database)?;
    let store_id: String = connection.query_row(
        "SELECT store_id FROM store_metadata WHERE singleton = 1",
        [],
        |row| row.get(0),
    )?;
    let schema: i64 = connection.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?;
    let repository = ProjectRepository::new(&mut connection);
    let path_mappings = ProjectPathMappings::new(repository.list_paths()?)?;
    let remote_mappings = ProjectRemoteMappings::new(repository.list_remotes()?)?;
    let working_directory = env::current_dir()?;
    let git = git_metadata(&working_directory);
    let project = resolve_project(
        None,
        &working_directory,
        git.as_ref(),
        &path_mappings,
        &remote_mappings,
    )?;
    let source = config
        .source
        .as_deref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "environment/command line".to_owned());
    Ok(format!(
        "version: {}\nconfig: {source}\nstore-id: {store_id}\nstore-path: {}\nschema-version: {schema}\nproject: {project}\n",
        env!("CARGO_PKG_VERSION"),
        paths.database.display()
    ))
}

pub(crate) fn git_metadata(working_directory: &Path) -> Option<GitMetadata> {
    let output = ProcessCommand::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(working_directory)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let repository_root = PathBuf::from(String::from_utf8(output.stdout).ok()?.trim());
    let output = ProcessCommand::new("git")
        .args(["remote", "-v"])
        .current_dir(&repository_root)
        .output()
        .ok()?;
    let remotes = String::from_utf8(output.stdout)
        .ok()?
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            Some(GitRemote {
                name: fields.next()?.to_owned(),
                target: fields.next()?.to_owned(),
            })
        })
        .collect();
    Some(GitMetadata {
        repository_root,
        remotes,
    })
}

fn selected_config_path(explicit: Option<&Path>) -> Result<PathBuf, CliError> {
    if let Some(path) = explicit {
        return Ok(path.to_owned());
    }
    if let Some(path) = env::var_os("BIF_CONFIG") {
        return Ok(PathBuf::from(path));
    }
    if cfg!(target_os = "windows") {
        env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|path| path.join("BIF").join("config.toml"))
    } else if cfg!(target_os = "macos") {
        env::var_os("HOME").map(PathBuf::from).map(|path| {
            path.join("Library")
                .join("Application Support")
                .join("BIF")
                .join("config.toml")
        })
    } else {
        env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|path| PathBuf::from(path).join(".config")))
            .map(|path| path.join("bif").join("config.toml"))
    }
    .ok_or_else(|| CliError::Operation("cannot determine the OS configuration path".into()))
}

fn config_string(path: &Path) -> Result<String, CliError> {
    let value = path
        .to_str()
        .ok_or_else(|| CliError::Operation("store root is not valid UTF-8".into()))?;
    if value.contains(['"', '\n', '\r']) {
        return Err(CliError::Operation(
            "store root cannot contain quotes or newlines".into(),
        ));
    }
    Ok(value.to_owned())
}

#[derive(Debug)]
pub(crate) enum CliError {
    Usage(String),
    Operation(String),
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) | Self::Operation(message) => formatter.write_str(message),
        }
    }
}

impl<E> From<E> for CliError
where
    E: std::error::Error,
{
    fn from(error: E) -> Self {
        Self::Operation(error.to_string())
    }
}
