//! Read-only MCP stdio entrypoint. Configuration is resolved once at startup.

use std::{env, path::PathBuf, process::ExitCode};

use bif::{
    config::{self, ConfigOverrides},
    mcp,
};

fn main() -> ExitCode {
    match start() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("bif-mcp: {message}");
            ExitCode::FAILURE
        }
    }
}

fn start() -> Result<(), String> {
    let mut overrides = ConfigOverrides::default();
    let mut args = env::args_os().skip(1);
    while let Some(argument) = args.next() {
        let value = args
            .next()
            .ok_or("expected --config PATH, --root PATH, or --requester ID")?;
        match argument.to_str() {
            Some("--config") if overrides.config.is_none() => {
                overrides.config = Some(PathBuf::from(value))
            }
            Some("--root") if overrides.root.is_none() => {
                overrides.root = Some(PathBuf::from(value))
            }
            Some("--requester") if overrides.requester.is_none() => {
                overrides.requester =
                    Some(value.into_string().map_err(|_| "requester must be UTF-8")?);
            }
            _ => {
                return Err(
                    "expected --config PATH, --root PATH, or --requester ID (no duplicates)".into(),
                );
            }
        }
    }
    let config = config::load(overrides).map_err(|error| error.to_string())?;
    mcp::run_stdio(config).map_err(|error| error.to_string())
}
