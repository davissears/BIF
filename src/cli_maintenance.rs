//! Explicit-path offline maintenance; never uses configuration or ordinary opens.

use std::path::PathBuf;

use crate::{
    cli::CliError,
    storage::{journal::Generation, maintenance},
};

pub(crate) const HELP: &str = "
  bif maintenance backup --store DB --output BACKUP --offline
  bif maintenance restore --store DB --from BACKUP --preserve-later BACKUP --offline
  bif maintenance prune --store DB --expected-generation GEN --through N --offline
Operator prerequisite: stop/reap all readers/writers and disable restart/new launches.
--offline acknowledges this prerequisite; it is NOT proof of shutdown.
Keep directories trusted. Outputs must be new; restore supports current schema only.";

#[derive(Debug)]
pub(crate) enum MaintenanceCommand {
    Backup {
        store: PathBuf,
        output: PathBuf,
    },
    Restore {
        store: PathBuf,
        from: PathBuf,
        preserve_later: PathBuf,
    },
    Prune {
        store: PathBuf,
        expected_generation: Generation,
        through: i64,
    },
}

pub(crate) fn parse(arguments: &[String]) -> Result<MaintenanceCommand, CliError> {
    let Some(operation @ ("backup" | "restore" | "prune")) = arguments.first().map(String::as_str)
    else {
        return Err(CliError::Usage(
            "maintenance requires backup, restore or prune".into(),
        ));
    };
    let mut values = Vec::new();
    let mut offline = false;
    let mut index = 1;
    while index < arguments.len() {
        let option = &arguments[index];
        if option == "--offline" {
            if offline {
                return Err(CliError::Usage("duplicate --offline".into()));
            }
            offline = true;
            index += 1;
            continue;
        }
        let allowed = match operation {
            "backup" => &["--store", "--output"][..],
            "restore" => &["--store", "--from", "--preserve-later"][..],
            _ => &["--store", "--expected-generation", "--through"][..],
        };
        if !allowed.contains(&option.as_str()) {
            return Err(CliError::Usage(format!(
                "unknown maintenance option {option:?}"
            )));
        }
        if values.iter().any(|(key, _)| *key == option) {
            return Err(CliError::Usage(format!("duplicate {option}")));
        }
        let value = arguments
            .get(index + 1)
            .filter(|value| !value.is_empty() && !value.starts_with("--"))
            .ok_or_else(|| CliError::Usage(format!("{option} requires a value")))?;
        values.push((option, value));
        index += 2;
    }
    if !offline {
        return Err(CliError::Usage(
            "maintenance requires explicit --offline acknowledgement".into(),
        ));
    }
    let required = |name| {
        values
            .iter()
            .find(|(key, _)| key.as_str() == name)
            .map(|(_, value)| value.as_str())
            .ok_or_else(|| CliError::Usage(format!("maintenance requires {name}")))
    };
    let store = PathBuf::from(required("--store")?);
    if operation == "backup" {
        Ok(MaintenanceCommand::Backup {
            store,
            output: PathBuf::from(required("--output")?),
        })
    } else if operation == "restore" {
        Ok(MaintenanceCommand::Restore {
            store,
            from: PathBuf::from(required("--from")?),
            preserve_later: PathBuf::from(required("--preserve-later")?),
        })
    } else {
        let expected_generation =
            Generation::parse(required("--expected-generation")?).map_err(|_| {
                CliError::Usage(
                    "--expected-generation requires 32 lowercase hexadecimal characters".into(),
                )
            })?;
        let value = required("--through")?;
        let through = value
            .parse::<i64>()
            .ok()
            .filter(|_| value.bytes().all(|byte| byte.is_ascii_digit()))
            .ok_or_else(|| {
                CliError::Usage("--through requires an integer from 0 through i64::MAX".into())
            })?;
        Ok(MaintenanceCommand::Prune {
            store,
            expected_generation,
            through,
        })
    }
}

pub(crate) fn execute(command: MaintenanceCommand) -> Result<String, CliError> {
    match command {
        MaintenanceCommand::Backup { store, output } => {
            maintenance::backup(store, &output)?;
            Ok(format!("Verified backup: {}\n", output.display()))
        }
        MaintenanceCommand::Restore {
            store,
            from,
            preserve_later,
        } => {
            maintenance::restore(&store, from, &preserve_later)?;
            Ok(format!(
                "Verified restore: {}\nPreserved later store: {}\nSync inactive; bootstrap required. Restart all clients.\n",
                store.display(),
                preserve_later.display()
            ))
        }
        MaintenanceCommand::Prune {
            store,
            expected_generation,
            through,
        } => {
            maintenance::prune(&store, &expected_generation, through)?;
            Ok(format!(
                "Pruned journal through {through}: {}\n",
                store.display()
            ))
        }
    }
}
