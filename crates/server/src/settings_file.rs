//! The admin page's view of the settings sections, and saving them to the
//! config file.
//!
//! Saving rewrites only the changed section's keys with `toml_edit`, so
//! comments and the layout of a hand-edited file survive. The file is
//! replaced atomically (temporary file + rename): a crash never leaves half
//! a config behind.

use std::collections::BTreeSet;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use rustcloud_core::{audit, Error, Field, FieldKind, Result, SettingsSection};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::Mutex;
use toml_edit::{DocumentMut, Item, Table};

use crate::config::{Config, ConfigError};

/// Header of a config file created by the admin page.
const NEW_FILE_HEADER: &str = "\
# RustCloud configuration. Tables ([server], [accounts], …) can also be changed
# on the admin page; it keeps comments in this file. Restart after editing by hand.
";

/// One section as the admin page shows it.
#[derive(Debug, Serialize)]
pub struct SectionView {
    pub name: &'static str,
    pub fields: Vec<Field>,
    pub values: Value,
    /// Keys set by environment variables: shown, but not editable.
    pub locked: Vec<&'static str>,
}

/// All settings sections of the running server and the file they are saved to.
pub struct SettingsFile {
    path: PathBuf,
    sections: Vec<Arc<dyn SettingsSection>>,
    env_keys: BTreeSet<String>,
    /// One save at a time, so two saves can't overwrite each other's file.
    saving: Mutex<()>,
}

impl SettingsFile {
    /// Fails if the config file has top-level keys that belong to no section.
    pub fn new(config: &Config, sections: Vec<Arc<dyn SettingsSection>>) -> Result<Self, ConfigError> {
        let names: Vec<&str> = sections.iter().map(|section| section.name()).collect();
        config.check_unknown_keys(&names)?;
        Ok(Self { path: config.file.clone(), sections, env_keys: config.env_keys.clone(), saving: Mutex::new(()) })
    }

    pub fn sections(&self) -> Vec<SectionView> {
        self.sections.iter().map(|section| self.view(section.as_ref())).collect()
    }

    /// Validates new values of a section, writes them to the config file and
    /// applies them. Values of locked keys are ignored.
    pub async fn save(&self, name: &str, values: Value) -> Result<SectionView> {
        let section = self
            .sections
            .iter()
            .find(|section| section.name() == name)
            .ok_or_else(|| Error::not_found("SETTINGS_NOT_FOUND", "No such settings section"))?;
        let Value::Object(mut values) = values else {
            return Err(Error::bad_request("INVALID_SETTINGS", "Expected an object with settings"));
        };

        let locked = self.locked_keys(section.as_ref());
        let current = section.values();
        for key in &locked {
            values.insert((*key).to_owned(), current[*key].clone());
        }
        let checked = section.check(Value::Object(values))?;

        let _saving = self.saving.lock().await;
        self.write(name, &checked, &locked).await?;
        section.apply(checked)?;
        audit!("settings.save", section = name, "settings saved");
        Ok(self.view(section.as_ref()))
    }

    fn view(&self, section: &dyn SettingsSection) -> SectionView {
        SectionView {
            name: section.name(),
            fields: section.fields(),
            values: section.values(),
            locked: self.locked_keys(section),
        }
    }

    fn locked_keys(&self, section: &dyn SettingsSection) -> Vec<&'static str> {
        let prefix = format!("{}.", section.name());
        section
            .fields()
            .into_iter()
            .map(|field| field.key)
            .filter(|key| self.env_keys.contains(&format!("{prefix}{key}")))
            .collect()
    }

    /// Puts the section's values into the config file, keeping everything else.
    async fn write(&self, name: &str, values: &Value, locked: &[&str]) -> Result<()> {
        let text = match tokio::fs::read_to_string(&self.path).await {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => NEW_FILE_HEADER.to_owned(),
            Err(err) => return Err(err.into()),
        };
        let invalid_file = |problem: String| {
            Error::conflict(
                "CONFIG_FILE_INVALID",
                format!("{} can't be updated: {problem}. Fix it by hand.", self.path.display()),
            )
        };
        let mut doc: DocumentMut = text.parse().map_err(|err| invalid_file(format!("{err}")))?;
        let table = doc
            .entry(name)
            .or_insert_with(|| Item::Table(Table::new()))
            .as_table_mut()
            .ok_or_else(|| invalid_file(format!("`{name}` is not a table")))?;

        let fields = self
            .sections
            .iter()
            .find(|section| section.name() == name)
            .map(|section| section.fields())
            .unwrap_or_default();
        for (key, value) in values.as_object().into_iter().flatten() {
            if locked.contains(&key.as_str()) {
                continue;
            }
            // An empty limit is "no limit"; a missing key would be the default.
            let no_limit = value.is_null()
                && fields
                    .iter()
                    .any(|field| field.key == key && matches!(field.kind, FieldKind::Number { optional: true, .. }));
            let value = if no_limit { &Value::from(0) } else { value };
            match to_toml(value) {
                Some(new) => set_keeping_comments(table, key, new),
                None => {
                    table.remove(key);
                }
            }
        }
        self.replace_file(doc.to_string()).await
    }

    async fn replace_file(&self, text: String) -> Result<()> {
        if let Some(dir) = self.path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
            tokio::fs::create_dir_all(dir).await?;
        }
        let file_name = self.path.file_name().map(|name| name.to_string_lossy()).unwrap_or_default();
        let temp = self.path.with_file_name(format!(".{file_name}.tmp"));

        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        // The file may hold secrets (database password, later login provider keys).
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&temp).await?;
        tokio::io::AsyncWriteExt::write_all(&mut file, text.as_bytes()).await?;
        file.sync_all().await?;
        tokio::fs::rename(&temp, &self.path).await?;
        Ok(())
    }
}

/// Replaces a value but keeps the comment after it (`key = 1 # comment`).
fn set_keeping_comments(table: &mut Table, key: &str, mut new: toml_edit::Value) {
    if let Some(old) = table.get_mut(key).and_then(Item::as_value_mut) {
        *new.decor_mut() = old.decor().clone();
        *old = new;
    } else {
        table.insert(key, Item::Value(new));
    }
}

/// A JSON setting value as TOML; `None` for `null` (the key is removed).
fn to_toml(value: &Value) -> Option<toml_edit::Value> {
    Some(match value {
        Value::Null => return None,
        Value::Bool(b) => (*b).into(),
        Value::Number(n) => match n.as_i64() {
            Some(i) => i.into(),
            None => n.as_f64().unwrap_or_default().into(),
        },
        Value::String(s) => s.as_str().into(),
        Value::Array(items) => toml_edit::Value::Array(items.iter().filter_map(to_toml).collect::<toml_edit::Array>()),
        Value::Object(map) => {
            toml_edit::Value::InlineTable(map.iter().filter_map(|(k, v)| to_toml(v).map(|v| (k.as_str(), v))).collect())
        }
    })
}
