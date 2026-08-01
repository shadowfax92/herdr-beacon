use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use toml_edit::{value, ArrayOfTables, DocumentMut, Item, Table};
use uuid::Uuid;

use crate::herdr::{Herdr, HerdrClient};

const KEY: &str = "alt+u";
const COMMAND: &str = "shadowfax.beacon.jump-unread";
const DESCRIPTION: &str = "Jump to newest unread agent";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallOutcome {
    Updated { backup: Option<PathBuf> },
    Unchanged,
}

pub fn install_from_environment() -> Result<InstallOutcome> {
    let path = config_path_from_environment()?;
    let outcome = install(&path)?;
    let herdr = Herdr::from_environment();
    match &outcome {
        InstallOutcome::Updated { .. } => {
            herdr.reload_config()?;
            if let Err(error) = herdr.notify("Beacon is bound to Alt-U", None) {
                eprintln!("Beacon keybinding installed; confirmation not shown: {error}");
            }
        }
        InstallOutcome::Unchanged => {
            if let Err(error) = herdr.notify("Beacon keybinding is already installed", None) {
                eprintln!("Beacon keybinding is installed; confirmation not shown: {error}");
            }
        }
    }
    Ok(outcome)
}

pub fn config_path_from_environment() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("HERDR_CONFIG_PATH") {
        return Ok(PathBuf::from(path));
    }
    if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(path).join("herdr/config.toml"));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".config/herdr/config.toml"))
}

pub fn install(path: &Path) -> Result<InstallOutcome> {
    let target = resolve_target(path)?;
    let original = match fs::read_to_string(&target) {
        Ok(contents) => Some(contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", target.display()))
        }
    };
    let mut document = match &original {
        Some(contents) => contents
            .parse::<DocumentMut>()
            .with_context(|| format!("failed to parse {}", target.display()))?,
        None => DocumentMut::new(),
    };

    ensure_builtin_key_is_available(&document)?;
    let commands = commands_mut(&mut document)?;
    if commands.iter().any(|table| {
        table_string(table, "key") == Some(KEY) && table_string(table, "command") != Some(COMMAND)
    }) {
        bail!("{KEY} is already bound to another custom command");
    }

    let beacon_commands = commands
        .iter()
        .filter(|table| table_string(table, "command") == Some(COMMAND))
        .collect::<Vec<_>>();
    if beacon_commands.len() == 1 && is_desired(beacon_commands[0]) {
        return Ok(InstallOutcome::Unchanged);
    }

    commands.retain(|table| table_string(table, "command") != Some(COMMAND));
    commands.push(beacon_table());
    let rendered = document.to_string();
    if original.as_deref() == Some(rendered.as_str()) {
        return Ok(InstallOutcome::Unchanged);
    }

    let parent = target
        .parent()
        .context("Herdr config path has no parent directory")?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    let backup = original
        .as_ref()
        .map(|contents| write_backup(&target, contents))
        .transpose()?;
    let mode = fs::metadata(&target)
        .map(|metadata| metadata.permissions().mode() & 0o777)
        .unwrap_or(0o600);
    atomic_write(&target, rendered.as_bytes(), mode)?;
    Ok(InstallOutcome::Updated { backup })
}

fn resolve_target(path: &Path) -> Result<PathBuf> {
    match fs::canonicalize(path) {
        Ok(target) => {
            if !fs::metadata(&target)?.is_file() {
                bail!("Herdr config is not a regular file");
            }
            Ok(target)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(path.to_path_buf()),
        Err(error) => Err(error).with_context(|| format!("failed to resolve {}", path.display())),
    }
}

fn commands_mut(document: &mut DocumentMut) -> Result<&mut ArrayOfTables> {
    let keys = document
        .as_table_mut()
        .entry("keys")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()
        .context("keys must be a TOML table")?;
    keys.entry("command")
        .or_insert_with(|| Item::ArrayOfTables(ArrayOfTables::new()))
        .as_array_of_tables_mut()
        .context("keys.command must be an array of tables")
}

fn table_string<'a>(table: &'a Table, key: &str) -> Option<&'a str> {
    table.get(key).and_then(Item::as_str)
}

fn ensure_builtin_key_is_available(document: &DocumentMut) -> Result<()> {
    let Some(keys) = document.get("keys").and_then(Item::as_table) else {
        return Ok(());
    };
    for (name, item) in keys.iter().filter(|(name, _)| *name != "command") {
        let occupied = item.as_str() == Some(KEY)
            || item
                .as_array()
                .is_some_and(|array| array.iter().any(|value| value.as_str() == Some(KEY)));
        if occupied {
            bail!("{KEY} is already assigned to keys.{name}");
        }
    }
    Ok(())
}

fn is_desired(table: &Table) -> bool {
    table.len() == 4
        && table_string(table, "key") == Some(KEY)
        && table_string(table, "type") == Some("plugin_action")
        && table_string(table, "command") == Some(COMMAND)
        && table_string(table, "description") == Some(DESCRIPTION)
}

fn beacon_table() -> Table {
    let mut table = Table::new();
    table.insert("key", value(KEY));
    table.insert("type", value("plugin_action"));
    table.insert("command", value(COMMAND));
    table.insert("description", value(DESCRIPTION));
    table
}

fn write_backup(target: &Path, contents: &str) -> Result<PathBuf> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_millis();
    let backup = target.with_file_name(format!(
        "{}.beacon-backup-{timestamp}-{}",
        target
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("config.toml"),
        Uuid::new_v4()
    ));
    let mode = fs::metadata(target)?.permissions().mode() & 0o777;
    write_new(&backup, contents.as_bytes(), mode)?;
    Ok(backup)
}

fn atomic_write(target: &Path, contents: &[u8], mode: u32) -> Result<()> {
    let parent = target
        .parent()
        .context("Herdr config path has no parent directory")?;
    let temporary = parent.join(format!(".beacon-{}.tmp", Uuid::new_v4()));
    let result: Result<()> = (|| {
        write_new(&temporary, contents, mode)?;
        fs::rename(&temporary, target)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.with_context(|| format!("failed to update {}", target.display()))
}

fn write_new(path: &Path, contents: &[u8], mode: u32) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)?;
    file.write_all(contents)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use tempfile::tempdir;
    use toml_edit::{DocumentMut, Item, Table};

    use super::*;

    fn write_config(contents: &str) -> (tempfile::TempDir, PathBuf) {
        let directory = tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(&path, contents).unwrap();
        (directory, path)
    }

    fn commands(path: &Path) -> Vec<(String, String)> {
        let document = fs::read_to_string(path)
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        document["keys"]["command"]
            .as_array_of_tables()
            .unwrap()
            .iter()
            .map(|table: &Table| {
                (
                    table.get("key").and_then(Item::as_str).unwrap().to_string(),
                    table
                        .get("command")
                        .and_then(Item::as_str)
                        .unwrap()
                        .to_string(),
                )
            })
            .collect()
    }

    #[test]
    fn install_preserves_unrelated_config_and_backs_up_the_original() {
        let original = r#"[theme]
name = "gruvbox"

[keys]
prefix = "ctrl+a"

[[keys.command]]
key = "alt+i"
type = "plugin_action"
command = "shadowfax.scratch.toggle-nvim"
description = "Scratch"
"#;
        let (_directory, path) = write_config(original);

        let InstallOutcome::Updated {
            backup: Some(backup),
        } = install(&path).unwrap()
        else {
            panic!("expected an updated config with a backup");
        };

        assert_eq!(fs::read_to_string(backup).unwrap(), original);
        assert_eq!(
            commands(&path),
            vec![
                ("alt+i".into(), "shadowfax.scratch.toggle-nvim".into()),
                (KEY.into(), COMMAND.into()),
            ]
        );
    }

    #[test]
    fn repeated_install_is_a_byte_for_byte_no_op() {
        let (_directory, path) = write_config("[keys]\nprefix = \"ctrl+a\"\n");
        install(&path).unwrap();
        let once = fs::read(&path).unwrap();
        let file_count = fs::read_dir(path.parent().unwrap()).unwrap().count();

        assert_eq!(install(&path).unwrap(), InstallOutcome::Unchanged);
        assert_eq!(fs::read(&path).unwrap(), once);
        assert_eq!(
            fs::read_dir(path.parent().unwrap()).unwrap().count(),
            file_count
        );
    }

    #[test]
    fn install_replaces_stale_or_duplicate_beacon_commands() {
        let (_directory, path) = write_config(
            r#"[keys]
[[keys.command]]
key = "alt+b"
type = "plugin_action"
command = "shadowfax.beacon.jump-unread"
description = "Old"

[[keys.command]]
key = "alt+n"
type = "plugin_action"
command = "shadowfax.beacon.jump-unread"
description = "Duplicate"
"#,
        );

        install(&path).unwrap();

        assert_eq!(commands(&path), vec![(KEY.into(), COMMAND.into())]);
    }

    #[test]
    fn install_refuses_an_existing_custom_alt_u_binding() {
        let original = r#"[keys]
[[keys.command]]
key = "alt+u"
type = "plugin_action"
command = "someone.else.open"
description = "Keep me"
"#;
        let (_directory, path) = write_config(original);

        let error = install(&path).unwrap_err();

        assert!(error.to_string().contains("already bound"));
        assert_eq!(fs::read_to_string(path).unwrap(), original);
    }

    #[test]
    fn install_refuses_a_builtin_alt_u_binding() {
        let original = "[keys]\nnext_agent = [\"ctrl+alt+j\", \"alt+u\"]\n";
        let (_directory, path) = write_config(original);

        let error = install(&path).unwrap_err();

        assert!(error.to_string().contains("keys.next_agent"));
        assert_eq!(fs::read_to_string(path).unwrap(), original);
    }

    #[test]
    fn install_can_create_a_new_config() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("nested/config.toml");

        assert_eq!(
            install(&path).unwrap(),
            InstallOutcome::Updated { backup: None }
        );
        assert_eq!(commands(&path), vec![(KEY.into(), COMMAND.into())]);
    }
}
