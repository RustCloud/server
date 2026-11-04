//! Settings that an administrator changes while the server runs.
//!
//! A module describes its settings as a plain struct implementing [`Settings`]:
//! one table in the config file (`[accounts]`, `[files]`, …) and one section on
//! the admin page. The module reads the current values through [`Live`], which
//! the server updates in place when the section is saved. Future apps (calls,
//! login providers, …) add their own sections the same way; the server and the
//! web UI need no changes for that, because the UI draws the form from [`Field`]s.

use std::sync::RwLock;

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use crate::{Error, Result};

/// One section of settings.
///
/// Implementations should use `#[serde(default, deny_unknown_fields)]`, so a
/// partial table in the config file works and a typo is reported.
pub trait Settings: Serialize + DeserializeOwned + Default + Clone + Send + Sync + 'static {
    /// Table name in the config file and section id in the API, e.g. `"accounts"`.
    const SECTION: &'static str;

    /// The fields shown on the settings page, in display order.
    fn fields() -> Vec<Field>;

    /// Checks what the types alone don't, such as ranges and URL formats.
    fn validate(&self) -> Result<()> {
        Ok(())
    }
}

/// A setting as the settings page shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Field {
    /// Key in the section (and in the config file).
    pub key: &'static str,
    #[serde(flatten)]
    pub kind: FieldKind,
}

/// How a setting is edited.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FieldKind {
    /// On/off.
    Bool,
    /// A whole number, e.g. days or MiB. An `optional` number may be left
    /// empty, which means "no limit".
    Number { unit: Option<&'static str>, optional: bool },
    /// A line of text.
    Text,
    /// A list of strings, one per line.
    List,
}

impl Field {
    pub fn bool(key: &'static str) -> Self {
        Self { key, kind: FieldKind::Bool }
    }

    pub fn number(key: &'static str, unit: &'static str) -> Self {
        Self { key, kind: FieldKind::Number { unit: Some(unit), optional: false } }
    }

    /// A number that may be empty ("no limit"). The config file can't hold
    /// an empty value, so "no limit" is saved there as `0`: the field must
    /// read `0` as `None`, with `#[serde(deserialize_with = "zero_is_none")]`.
    pub fn optional_number(key: &'static str, unit: &'static str) -> Self {
        Self { key, kind: FieldKind::Number { unit: Some(unit), optional: true } }
    }

    pub fn text(key: &'static str) -> Self {
        Self { key, kind: FieldKind::Text }
    }

    pub fn list(key: &'static str) -> Self {
        Self { key, kind: FieldKind::List }
    }
}

/// Reads an optional limit where `0` (and `null`) mean "no limit"; see
/// [`Field::optional_number`].
pub fn zero_is_none<'de, D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Option<u64>, D::Error> {
    let value: Option<u64> = serde::Deserialize::deserialize(deserializer)?;
    Ok(value.filter(|&n| n != 0))
}

/// Current values of a settings section, shared between the module that uses
/// them and the server that changes them.
#[derive(Debug, Default)]
pub struct Live<T>(RwLock<T>);

impl<T: Clone> Live<T> {
    pub fn new(value: T) -> Self {
        Self(RwLock::new(value))
    }

    /// A copy of the current values.
    pub fn get(&self) -> T {
        self.read(T::clone)
    }

    /// Looks at the current values without copying them.
    pub fn read<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        f(&self.0.read().unwrap_or_else(|poisoned| poisoned.into_inner()))
    }

    pub fn set(&self, value: T) {
        *self.0.write().unwrap_or_else(|poisoned| poisoned.into_inner()) = value;
    }
}

/// A settings section with its type erased, so the server can keep a list of
/// sections from different modules. Implemented for every `Live<T: Settings>`.
pub trait SettingsSection: Send + Sync {
    fn name(&self) -> &'static str;
    fn fields(&self) -> Vec<Field>;
    /// Current values as a JSON object.
    fn values(&self) -> Value;
    /// Parses and validates new values; returns them as a complete JSON object
    /// (missing keys filled with defaults). Nothing is applied yet.
    fn check(&self, values: Value) -> Result<Value>;
    /// Applies values that passed [`check`](Self::check).
    fn apply(&self, values: Value) -> Result<()>;
}

impl<T: Settings> SettingsSection for Live<T> {
    fn name(&self) -> &'static str {
        T::SECTION
    }

    fn fields(&self) -> Vec<Field> {
        T::fields()
    }

    fn values(&self) -> Value {
        self.read(|value| serde_json::to_value(value)).unwrap_or(Value::Null)
    }

    fn check(&self, values: Value) -> Result<Value> {
        let parsed = parse::<T>(values)?;
        serde_json::to_value(parsed).map_err(|err| Error::internal(format!("settings serialisation failed: {err}")))
    }

    fn apply(&self, values: Value) -> Result<()> {
        self.set(parse::<T>(values)?);
        Ok(())
    }
}

fn parse<T: Settings>(values: Value) -> Result<T> {
    let parsed: T =
        serde_json::from_value(values).map_err(|err| Error::bad_request("INVALID_SETTINGS", err.to_string()))?;
    parsed.validate()?;
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;
    use serde_json::json;

    use super::*;

    #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
    #[serde(default, deny_unknown_fields)]
    struct Example {
        enabled: bool,
        limit: Option<u32>,
    }

    impl Settings for Example {
        const SECTION: &'static str = "example";

        fn fields() -> Vec<Field> {
            vec![Field::bool("enabled"), Field::optional_number("limit", "MiB")]
        }

        fn validate(&self) -> Result<()> {
            match self.limit {
                Some(0) => Err(Error::bad_request("INVALID_SETTINGS", "limit must be positive")),
                _ => Ok(()),
            }
        }
    }

    #[test]
    fn check_fills_defaults_and_apply_changes_the_live_value() {
        let live = Live::new(Example::default());
        let checked = live.check(json!({ "enabled": true })).expect("valid");
        assert_eq!(checked, json!({ "enabled": true, "limit": null }));
        assert!(!live.get().enabled, "check alone changes nothing");

        live.apply(checked).expect("valid");
        assert_eq!(live.get(), Example { enabled: true, limit: None });
        assert_eq!(live.values(), json!({ "enabled": true, "limit": null }));
    }

    #[test]
    fn check_rejects_unknown_keys_and_invalid_values() {
        let live = Live::new(Example::default());
        assert!(live.check(json!({ "enabeld": true })).is_err(), "typo");
        assert!(live.check(json!({ "limit": "ten" })).is_err(), "wrong type");
        assert!(live.check(json!({ "limit": 0 })).is_err(), "fails validate()");
    }

    #[test]
    fn zero_reads_as_no_limit() {
        #[derive(Deserialize)]
        struct Limit {
            #[serde(default = "ten", deserialize_with = "zero_is_none")]
            limit: Option<u64>,
        }
        fn ten() -> Option<u64> {
            Some(10)
        }
        let read = |value| serde_json::from_value::<Limit>(value).expect("valid").limit;
        assert_eq!(read(json!({ "limit": 0 })), None);
        assert_eq!(read(json!({ "limit": null })), None);
        assert_eq!(read(json!({ "limit": 5 })), Some(5));
        assert_eq!(read(json!({})), Some(10), "missing is the default");
    }

    #[test]
    fn fields_are_described_for_the_ui() {
        let field = serde_json::to_value(Field::optional_number("limit", "MiB")).expect("serialisable");
        assert_eq!(field, json!({ "key": "limit", "kind": "number", "unit": "MiB", "optional": true }));
    }
}
