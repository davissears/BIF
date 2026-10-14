//! Explicit-path offline maintenance; never uses configuration or ordinary opens.

use std::path::PathBuf;

use crate::{cli::CliError, storage::maintenance};

pub(crate) const HELP: &str = "
  bif maintenance backup --store DB --output BACKUP --offline
  bif maintenance restore --store DB --from BACKUP --preserve-later BACKUP --offline
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
}

pub(crate) fn parse(arguments: &[String]) -> Result<MaintenanceCommand, CliError> {
    let Some(operation @ ("backup" | "restore")) = arguments.first().map(String::as_str) else {
        return Err(CliError::Usage(
            "maintenance requires backup or restore".into(),
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
        let allowed = if operation == "backup" {
            &["--store", "--output"][..]
        } else {
            &["--store", "--from", "--preserve-later"][..]
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
            .ok_or_else(|| CliError::Usage(format!("{option} requires a path")))?;
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
            .map(|(_, value)| PathBuf::from(value))
            .ok_or_else(|| CliError::Usage(format!("maintenance requires {name}")))
    };
    let store = required("--store")?;
    if operation == "backup" {
        Ok(MaintenanceCommand::Backup {
            store,
            output: required("--output")?,
        })
    } else {
        Ok(MaintenanceCommand::Restore {
            store,
            from: required("--from")?,
            preserve_later: required("--preserve-later")?,
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
    }
}
