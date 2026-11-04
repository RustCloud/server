//! Server configuration.
//!
//! Everything lives in one TOML file. Top-level keys ([`Startup`]) are read
//! once at startup. Each table — `[server]`, `[accounts]`, `[files]`, and the
//! tables of future apps — is a settings section that administrators can also
//! change on the admin page, which writes the file back (see
//! [`SettingsFile`](crate::settings_file::SettingsFile)). If a change through
//! the web UI locks you out, fix the file by hand and restart.
//!
//! Sources, later ones win:
//! 1. built-in defaults (a ready-to-run single-user setup with SQLite);
//! 2. the config file (`--config`, `rustcloud.toml` by default; it may not exist yet);
//! 3. environment variables: `RUSTCLOUD_<KEY>` for startup settings and
//!    `RUSTCLOUD_<SECTION>__<KEY>` for sections, e.g.
//!    `RUSTCLOUD_ACCOUNTS__OPEN_REGISTRATION=true`. A setting given by the
//!    environment can't be changed on the admin page.
//!
//! Unknown keys are errors, so a typo is reported instead of silently ignored.

use crate::plugins::Source;
use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use figment::providers::{Env, Format, Toml};
use figment::{Figment, Provider};
use rustcloud_core::Settings;
use serde::{Deserialize, Serialize};

/// Error while loading the configuration.
pub type ConfigError = Box<figment::Error>;

/// Settings read once at startup; changing them needs a restart.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Startup {
    /// Address and port to listen on.
    pub listen: SocketAddr,
    /// `postgres://user:password@host/db` or `sqlite://path/file.db?mode=rwc`.
    pub database_url: String,
    /// Where file contents are stored.
    pub data_dir: PathBuf,
    /// Log filter, e.g. `info` or `rustcloud=debug,info`. `RUST_LOG` overrides it.
    pub log: String,
    /// WASM apps that ship with the server (the Docker image sets
    /// `/usr/share/rustcloud/apps`); `None`: none. Apps installed by the
    /// administrator live in `<data_dir>/apps`.
    pub builtin_apps_dir: Option<PathBuf>,
    /// The static ffmpeg and ffprobe plugins may `use` (the Docker image sets
    /// `/usr/libexec/rustcloud`); `None`: the server has none.
    pub media_tools_dir: Option<PathBuf>,
}

impl Startup {
    const KEYS: [&str; 6] = ["listen", "database_url", "data_dir", "log", "builtin_apps_dir", "media_tools_dir"];

    /// Folders to read WASM apps from: the shipped ones first.
    pub fn apps_dirs(&self) -> Vec<(PathBuf, Source)> {
        let shipped = self.builtin_apps_dir.iter().map(|dir| (dir.clone(), Source::Shipped));
        shipped.chain([(self.installed_apps_dir(), Source::Installed)]).collect()
    }

    /// Where apps installed by the administrator go.
    pub fn installed_apps_dir(&self) -> PathBuf {
        self.data_dir.join("apps")
    }
}

impl Default for Startup {
    fn default() -> Self {
        Self {
            listen: SocketAddr::from(([0, 0, 0, 0], 8080)),
            database_url: "sqlite://data/rustcloud.db?mode=rwc".into(),
            data_dir: PathBuf::from("data"),
            log: "info".into(),
            builtin_apps_dir: None,
            media_tools_dir: None,
        }
    }
}

/// Settings that moved into sections in 0.1.0-alpha.2, for a helpful error.
const MOVED_KEYS: [(&str, &str); 4] = [
    ("public_url", "server"),
    ("open_registration", "accounts"),
    ("session_days", "accounts"),
    ("default_quota_mib", "accounts"),
];

/// The loaded configuration: startup settings, plus the merged sources from
/// which the modules read their sections.
pub struct Config {
    pub startup: Startup,
    /// The config file; the admin page saves settings here.
    pub file: PathBuf,
    /// `section.key` of every section setting given by an environment variable.
    pub env_keys: BTreeSet<String>,
    figment: Figment,
}

impl Config {
    /// Loads the configuration from defaults, `file` (if it exists) and the environment.
    pub fn load(file: &Path) -> Result<Self, ConfigError> {
        // RUSTCLOUD_CONFIG names the file itself; RUSTCLOUD_TEST_POSTGRES belongs to the tests;
        // RUSTCLOUD_ADMIN_* create the first administrator (see `main`).
        let env = Env::prefixed("RUSTCLOUD_").split("__").ignore(&[
            "config",
            "test_postgres",
            "admin_username",
            "admin_password",
        ]);
        let env_keys = env.iter().map(|(key, _)| key.as_str().to_owned()).filter(|key| key.contains('.')).collect();

        let figment = Figment::from(Toml::file(file)).merge(env);
        let startup = figment.extract()?;
        Ok(Self { startup, file: file.to_owned(), env_keys, figment })
    }

    /// The values of one settings section (defaults for whatever is not set).
    pub fn section<T: Settings>(&self) -> Result<T, ConfigError> {
        let value: T =
            if self.figment.contains(T::SECTION) { self.figment.extract_inner(T::SECTION)? } else { T::default() };
        value.validate().map_err(|err| config_error(format!("[{}]: {}", T::SECTION, err.message)))?;
        Ok(value)
    }

    /// Fails on top-level keys that are neither startup settings nor one of `sections`.
    pub fn check_unknown_keys(&self, sections: &[&str]) -> Result<(), ConfigError> {
        let data = self.figment.data()?;
        for key in data.values().flat_map(|dict| dict.keys()) {
            if Startup::KEYS.contains(&key.as_str()) || sections.contains(&key.as_str()) {
                continue;
            }
            let hint = match MOVED_KEYS.iter().find(|(old, _)| old == key) {
                Some((_, section)) => format!(
                    "; it moved to the [{section}] table (environment: RUSTCLOUD_{}__{})",
                    section.to_uppercase(),
                    key.to_uppercase()
                ),
                None => String::new(),
            };
            return Err(config_error(format!("unknown setting `{key}`{hint}")));
        }
        Ok(())
    }
}

fn config_error(message: String) -> ConfigError {
    Box::new(figment::Error::from(message))
}

#[cfg(test)]
#[allow(clippy::result_large_err)] // figment::Jail closures return figment's own Result
mod tests {
    use rustcloud_auth::AccountSettings;

    use crate::ServerSettings;

    use super::*;

    #[test]
    fn file_and_environment_override_defaults() {
        figment::Jail::expect_with(|jail| {
            jail.create_file("rustcloud.toml", "log = \"debug\"\n[accounts]\nsession_days = 2")?;
            jail.set_env("RUSTCLOUD_ACCOUNTS__OPEN_REGISTRATION", "true");
            jail.set_env("RUSTCLOUD_CONFIG", "rustcloud.toml");
            // RUSTCLOUD_TEST_POSTGRES is ignored too, but setting it here would race
            // with tests that open a database; the PostgreSQL CI job covers it.

            let config = Config::load(Path::new("rustcloud.toml")).expect("valid config");
            assert_eq!(config.startup.log, "debug");
            assert_eq!(config.startup.listen, Startup::default().listen);
            config.check_unknown_keys(&["accounts"]).expect("no unknown keys");

            let accounts: AccountSettings = config.section().expect("valid section");
            assert_eq!(accounts.session_days, 2);
            assert!(accounts.open_registration);
            assert_eq!(config.env_keys, BTreeSet::from(["accounts.open_registration".to_owned()]));
            Ok(())
        });
    }

    #[test]
    fn lists_can_be_set_by_environment_variables() {
        figment::Jail::expect_with(|jail| {
            // The form compose.yml shows.
            jail.set_env("RUSTCLOUD_SERVER__TRUSTED_PROXIES", r#"["172.16.0.0/12", "10.0.0.1"]"#);
            let config = Config::load(Path::new("missing.toml")).expect("loads");
            let server: ServerSettings = config.section().expect("valid section");
            assert_eq!(server.trusted_proxies, ["172.16.0.0/12", "10.0.0.1"]);
            Ok(())
        });
    }

    #[test]
    fn a_missing_file_gives_defaults() {
        figment::Jail::expect_with(|_jail| {
            let config = Config::load(Path::new("missing.toml")).expect("defaults");
            assert_eq!(config.section::<AccountSettings>().expect("defaults"), AccountSettings::default());
            Ok(())
        });
    }

    #[test]
    fn typos_and_invalid_values_are_errors() {
        figment::Jail::expect_with(|jail| {
            jail.create_file("typo.toml", "listne = \"0.0.0.0:1\"")?;
            let config = Config::load(Path::new("typo.toml")).expect("loads");
            assert!(config.check_unknown_keys(&[]).is_err(), "unknown top-level key");

            jail.create_file("section.toml", "[accounts]\nsesion_days = 1")?;
            let config = Config::load(Path::new("section.toml")).expect("loads");
            assert!(config.section::<AccountSettings>().is_err(), "unknown key in a section");

            jail.create_file("range.toml", "[accounts]\nsession_days = 0")?;
            let config = Config::load(Path::new("range.toml")).expect("loads");
            assert!(config.section::<AccountSettings>().is_err(), "fails validation");
            Ok(())
        });
    }

    #[test]
    fn old_flat_keys_point_to_their_new_table() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("RUSTCLOUD_PUBLIC_URL", "https://cloud.example.com");
            let config = Config::load(Path::new("none.toml")).expect("loads");
            let err = config.check_unknown_keys(&["server"]).expect_err("moved key");
            assert!(err.to_string().contains("RUSTCLOUD_SERVER__PUBLIC_URL"), "{err}");
            Ok(())
        });
    }
}
