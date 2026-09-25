use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use toml_edit::{value, ArrayOfTables, DocumentMut, Item, Table};
use uuid::Uuid;

use crate::herdr::{Herdr, HerdrClient};

const UNREAD_KEY: &str = "alt+u";
const UNREAD_COMMAND: &str = "shadowfax.beacon.jump-unread";
const UNREAD_DESCRIPTION: &str = "Cycle through unread or blocked agents";
const WORKING_KEY: &str = "alt+o";
const WORKING_COMMAND: &str = "shadowfax.beacon.jump-working";
const WORKING_DESCRIPTION: &str = "Cycle through working or blocked agents";
const RECENT_KEY: &str = "alt+quote";
const RECENT_COMMAND: &str = "shadowfax.beacon.jump-recent";
const RECENT_DESCRIPTION: &str = "Cycle through idle, done, or unknown agents by recent activity";
const REVERSE_KEY: &str = "alt+shift+quote";
const REVERSE_LEGACY_KEY: &str = "alt+double_quote";
const REVERSE_COMMAND: &str = "shadowfax.beacon.jump-recent-reverse";
const REVERSE_DESCRIPTION: &str =
    "Cycle backward through idle, done, or unknown agents by recent activity";

/// One direct Herdr shortcut owned and normalized by Beacon's installer.
struct Binding {
    key: &'static str,
    command: &'static str,
    description: &'static str,
}

// This is the complete Beacon-owned set. Installation validates every destination
// before rewriting any entry so a conflict cannot leave a partially upgraded config.
const BINDINGS: [Binding; 5] = [
    Binding {
        key: UNREAD_KEY,
        command: UNREAD_COMMAND,
        description: UNREAD_DESCRIPTION,
    },
    Binding {
        key: WORKING_KEY,
        command: WORKING_COMMAND,
        description: WORKING_DESCRIPTION,
    },
    Binding {
        key: RECENT_KEY,
        command: RECENT_COMMAND,
        description: RECENT_DESCRIPTION,
    },
    Binding {
        key: REVERSE_KEY,
        command: REVERSE_COMMAND,
        description: REVERSE_DESCRIPTION,
    },
    // Legacy terminals send Alt-double-quote without a separate Shift modifier.
    // Both encodings invoke one action; neither is a second navigation step.
    Binding {
        key: REVERSE_LEGACY_KEY,
        command: REVERSE_COMMAND,
        description: REVERSE_DESCRIPTION,
    },
];

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
            if let Err(error) = herdr.notify(
                "Beacon shortcuts installed, including Shift-Alt-' to go back",
                None,
            ) {
                eprintln!("Beacon keybinding installed; confirmation not shown: {error}");
            }
        }
        InstallOutcome::Unchanged => {
            if let Err(error) = herdr.notify("Beacon keybindings are already installed", None) {
                eprintln!("Beacon keybindings are installed; confirmation not shown: {error}");
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

    ensure_builtin_keys_are_available(&document)?;
    let commands = commands_mut(&mut document)?;
    for binding in &BINDINGS {
        if commands.iter().any(|table| {
            table
                .get("key")
                .is_some_and(|item| item_uses_key(item, binding.key))
                && table_string(table, "command") != Some(binding.command)
        }) {
            bail!("{} is already bound to another custom command", binding.key);
        }
    }

    // One action can own multiple terminal encodings. Require each exact block
    // once and no extra owned blocks, so upgrades remove stale bindings and a
    // second install remains byte-for-byte unchanged.
    let owned_count = commands
        .iter()
        .filter(|table| {
            BINDINGS
                .iter()
                .any(|binding| table_string(table, "command") == Some(binding.command))
        })
        .count();
    let all_bindings_are_current = owned_count == BINDINGS.len()
        && BINDINGS.iter().all(|binding| {
            commands
                .iter()
                .filter(|table| is_desired(table, binding))
                .count()
                == 1
        });
    if all_bindings_are_current {
        return Ok(InstallOutcome::Unchanged);
    }

    commands.retain(|table| {
        !BINDINGS
            .iter()
            .any(|binding| table_string(table, "command") == Some(binding.command))
    });
    for binding in &BINDINGS {
        commands.push(beacon_table(binding));
    }
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

fn ensure_builtin_keys_are_available(document: &DocumentMut) -> Result<()> {
    let Some(keys) = document.get("keys").and_then(Item::as_table) else {
        return Ok(());
    };
    for binding in &BINDINGS {
        for (name, item) in keys.iter().filter(|(name, _)| *name != "command") {
            let occupied = item_uses_key(item, binding.key);
            if occupied {
                bail!("{} is already assigned to keys.{name}", binding.key);
            }
        }
    }
    Ok(())
}

/// Both built-in and custom Herdr bindings accept a string or an array of
/// strings. Inspect every alias before touching the config so an array cannot
/// silently keep a conflicting shortcut and disable Beacon's new binding.
fn item_uses_key(item: &Item, expected: &str) -> bool {
    item.as_str().is_some_and(|key| key_matches(key, expected))
        || item.as_array().is_some_and(|array| {
            array
                .iter()
                .any(|value| value.as_str().is_some_and(|key| key_matches(key, expected)))
        })
}

/// The Alt-chord subset Beacon owns, normalized across Herdr modifier aliases
/// and shifted punctuation. This protects config writes from shadowing equivalent
/// user bindings; it is not a replacement for Herdr's general key parser.
#[derive(PartialEq, Eq)]
struct AltShortcut {
    key: char,
    shift: bool,
}

impl AltShortcut {
    fn parse(value: &str) -> Option<Self> {
        let mut has_alt = false;
        let mut shift = false;
        let mut key = None;
        for token in value.split('+').map(str::trim) {
            match token.to_ascii_lowercase().as_str() {
                "alt" | "option" | "meta" => has_alt = true,
                "shift" => shift = true,
                _ if key.is_some() => return None,
                "quote" => key = Some('\''),
                "double_quote" | "double-quote" => key = Some('"'),
                _ => {
                    let mut chars = token.chars();
                    key = Some(chars.next()?);
                    if chars.next().is_some() {
                        return None;
                    }
                }
            }
        }
        if !has_alt {
            return None;
        }
        let mut key = key?;
        // Uppercase letters mean Shift in Herdr; punctuation may instead arrive
        // as its shifted character, with or without an explicit Shift bit.
        if key.is_ascii_uppercase() {
            key = key.to_ascii_lowercase();
            shift = true;
        }
        if key == '\'' && shift {
            key = '"';
        }
        if key == '"' {
            shift = false;
        }
        Some(Self { key, shift })
    }
}

fn key_matches(actual: &str, expected: &str) -> bool {
    AltShortcut::parse(actual).is_some_and(|key| Some(key) == AltShortcut::parse(expected))
}

fn is_desired(table: &Table, binding: &Binding) -> bool {
    table.len() == 4
        && table_string(table, "key") == Some(binding.key)
        && table_string(table, "type") == Some("plugin_action")
        && table_string(table, "command") == Some(binding.command)
        && table_string(table, "description") == Some(binding.description)
}

fn beacon_table(binding: &Binding) -> Table {
    let mut table = Table::new();
    table.insert("key", value(binding.key));
    table.insert("type", value("plugin_action"));
    table.insert("command", value(binding.command));
    table.insert("description", value(binding.description));
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
                (UNREAD_KEY.into(), UNREAD_COMMAND.into()),
                (WORKING_KEY.into(), WORKING_COMMAND.into()),
                (RECENT_KEY.into(), RECENT_COMMAND.into()),
                (REVERSE_KEY.into(), REVERSE_COMMAND.into()),
                (REVERSE_LEGACY_KEY.into(), REVERSE_COMMAND.into()),
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

        assert_eq!(
            commands(&path),
            vec![
                (UNREAD_KEY.into(), UNREAD_COMMAND.into()),
                (WORKING_KEY.into(), WORKING_COMMAND.into()),
                (RECENT_KEY.into(), RECENT_COMMAND.into()),
                (REVERSE_KEY.into(), REVERSE_COMMAND.into()),
                (REVERSE_LEGACY_KEY.into(), REVERSE_COMMAND.into()),
            ]
        );
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
    fn install_refuses_an_existing_custom_alt_o_binding() {
        let original = r#"[keys]
[[keys.command]]
key = "alt+o"
type = "plugin_action"
command = "someone.else.open"
description = "Keep me"
"#;
        let (_directory, path) = write_config(original);

        let error = install(&path).unwrap_err();

        assert!(error.to_string().contains("alt+o is already bound"));
        assert_eq!(fs::read_to_string(path).unwrap(), original);
    }

    #[test]
    fn install_refuses_equivalent_alt_shortcuts_before_writing() {
        for key in [
            "alt+quote",
            "alt+'",
            "option+quote",
            "meta+'",
            "quote+alt",
            " ' + OPTION ",
            "ALT+META+QUOTE",
            "u+option",
            "META+o",
            "shift+option+quote",
            "SHIFT + ' + META",
            "meta+double_quote",
            "double-quote+alt",
            "alt+shift+double_quote",
        ] {
            for original in [
                format!("[keys]\nnext_agent = [\"{key}\"]\n"),
                format!("[keys]\n[[keys.command]]\nkey = \"{key}\"\ntype = \"plugin_action\"\ncommand = \"someone.else.open\"\n"),
            ] {
                let (_directory, path) = write_config(&original);
                assert!(install(&path).is_err(), "must detect {key}");
                assert_eq!(fs::read_to_string(path).unwrap(), original);
            }
        }
    }

    #[test]
    fn install_refuses_custom_key_arrays_without_writing_or_backing_up() {
        for key in [
            "shift+alt+quote",
            "option+double_quote",
            "META+SHIFT+'",
            "alt+u",
            "alt+o",
        ] {
            let original = format!("[keys]\n[[keys.command]]\nkey = [\"alt+z\", \"{key}\"]\ntype = \"plugin_action\"\ncommand = \"someone.else.open\"\n");
            let (directory, path) = write_config(&original);
            let error = install(&path).unwrap_err();
            assert!(error.to_string().contains("already bound"));
            assert_eq!(fs::read_to_string(&path).unwrap(), original);
            assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
        }
    }

    #[test]
    fn install_allows_distinct_shifted_or_modified_shortcuts() {
        for key in ["alt+U", "alt+O", "ctrl+alt+quote", "ctrl+alt+shift+quote"] {
            let original = format!("[keys]\nnext_agent = \"{key}\"\n");
            let (_directory, path) = write_config(&original);
            assert!(install(&path).is_ok(), "must preserve distinct {key}");
        }
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
        assert_eq!(
            commands(&path),
            vec![
                (UNREAD_KEY.into(), UNREAD_COMMAND.into()),
                (WORKING_KEY.into(), WORKING_COMMAND.into()),
                (RECENT_KEY.into(), RECENT_COMMAND.into()),
                (REVERSE_KEY.into(), REVERSE_COMMAND.into()),
                (REVERSE_LEGACY_KEY.into(), REVERSE_COMMAND.into()),
            ]
        );
    }
}
