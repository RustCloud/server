//! The plugin catalog: a list of plugins an administrator can install from
//! the internet, kept as one JSON file (`index.json`) in a git repository.
//!
//! ```json
//! {
//!   "schema": 1,
//!   "plugins": [{
//!     "id": "photo-info",
//!     "name": { "en": "Photo info", "ru": "Сведения о фото" },
//!     "description": { "en": "Shows what a photo is.", "ru": "Показывает, что на фото." },
//!     "author": "RustCloud",
//!     "license": "MIT OR Apache-2.0",
//!     "source": "https://github.com/RustCloud/photo-info",
//!     "versions": [{
//!       "version": "0.1.0",
//!       "api": "^0.2.16",
//!       "url": "https://github.com/RustCloud/photo-info/releases/download/v0.1.0/photo-info.zip",
//!       "sha256": "…64 hex digits…",
//!       "size": 123456
//!     }]
//!   }]
//! }
//! ```
//!
//! A server shows, for every plugin, the newest version that works with its
//! plugin API ([`CatalogPlugin::best_release`]). Before installing it checks
//! the archive against the release ([`Release::check_archive`],
//! [`Release::check_manifest`]); what the plugin may do comes from the
//! manifest in the archive, never from the catalog.
//!
//! One broken entry doesn't hide the others: [`Catalog::from_json`] skips it
//! and says why. Unknown fields are ignored, so later versions of the format
//! may add optional ones without changing [`SCHEMA`]. The format is
//! described for people in `docs/dev/plugin-catalog.md`.

use std::collections::{BTreeMap, BTreeSet};

use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};

use crate::Manifest;

/// The version of the format this server reads. A catalog with a bigger
/// one needs a newer server.
pub const SCHEMA: u32 = 1;

/// Largest plugin archive in a catalog; the same as a manual install allows.
pub const MAX_ARCHIVE_SIZE: u64 = 64 * 1024 * 1024;

/// Longest description in one language, in characters.
pub const MAX_DESCRIPTION: usize = 500;

/// A catalog as a server uses it: the valid entries and why others were skipped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Catalog {
    pub plugins: Vec<CatalogPlugin>,
    /// One line per skipped entry: which one and what is wrong with it.
    pub skipped: Vec<String>,
}

/// A plugin in the catalog with its published versions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogPlugin {
    /// The id in the plugin's manifest.
    pub id: String,
    /// By language code; `en` is required and is the fallback.
    pub name: BTreeMap<String, String>,
    /// A sentence or two, by language code; `en` is required.
    pub description: BTreeMap<String, String>,
    pub author: String,
    /// An SPDX expression, e.g. `MIT OR Apache-2.0`.
    pub license: String,
    /// Where its source code is: an `https://` address.
    pub source: String,
    /// A page about it, if not the source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    /// In any order; every version once.
    pub versions: Vec<Release>,
}

/// One published version of a plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    /// The `version` in the archive's manifest.
    pub version: Version,
    /// The `api` in the archive's manifest.
    pub api: VersionReq,
    /// The archive: an `https://` address, or one relative to the catalog
    /// file (a mirror copies the folder as is). It must never change: a
    /// release asset of a tag, not "latest".
    pub url: String,
    /// SHA-256 of the archive, lower-case hex.
    pub sha256: String,
    /// Size of the archive in bytes.
    pub size: u64,
    /// What changed, by language code.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub notes: BTreeMap<String, String>,
}

/// What is wrong with a catalog, an entry or an archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogError(pub String);

impl std::fmt::Display for CatalogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CatalogError {}

/// The catalog file before its entries are checked one by one.
#[derive(Deserialize)]
struct RawCatalog {
    schema: u32,
    plugins: Vec<serde_json::Value>,
}

impl Catalog {
    /// Reads `index.json`. Fails only if the file as a whole can't be read;
    /// invalid entries and repeated ids (after the first) are skipped.
    pub fn from_json(text: &str) -> Result<Self, CatalogError> {
        let raw: RawCatalog =
            serde_json::from_str(text).map_err(|err| CatalogError(format!("invalid catalog: {err}")))?;
        if raw.schema != SCHEMA {
            return Err(CatalogError(format!(
                "the catalog has format {}, this server reads format {SCHEMA}",
                raw.schema
            )));
        }
        let mut catalog = Self { plugins: Vec::new(), skipped: Vec::new() };
        let mut ids = BTreeSet::new();
        for (index, value) in raw.plugins.into_iter().enumerate() {
            let label = match value.get("id").and_then(|id| id.as_str()) {
                Some(id) => format!("\"{id}\""),
                None => format!("entry {}", index + 1),
            };
            let entry = serde_json::from_value::<CatalogPlugin>(value)
                .map_err(|err| CatalogError(err.to_string()))
                .and_then(|entry| entry.validate().map(|()| entry));
            match entry {
                Ok(entry) if !ids.insert(entry.id.clone()) => {
                    catalog.skipped.push(format!("{label}: listed more than once"));
                }
                Ok(entry) => catalog.plugins.push(entry),
                Err(err) => catalog.skipped.push(format!("{label}: {err}")),
            }
        }
        Ok(catalog)
    }
}

impl CatalogPlugin {
    /// Checks what the types alone don't.
    pub fn validate(&self) -> Result<(), CatalogError> {
        if !crate::manifest::valid_app_id(&self.id) {
            return Err(CatalogError("id must be 1–40 lower-case letters, digits or \"-\"".to_owned()));
        }
        for (field, texts) in [("name", &self.name), ("description", &self.description)] {
            if texts.get("en").is_none_or(|text| text.trim().is_empty()) {
                return Err(CatalogError(format!("{field}.en is required")));
            }
        }
        if self.description.values().any(|text| text.chars().count() > MAX_DESCRIPTION) {
            return Err(CatalogError(format!("a description is longer than {MAX_DESCRIPTION} characters")));
        }
        if self.author.trim().is_empty() || self.license.trim().is_empty() {
            return Err(CatalogError("author and license are required".to_owned()));
        }
        if !self.source.starts_with("https://")
            || self.homepage.as_ref().is_some_and(|url| !url.starts_with("https://"))
        {
            return Err(CatalogError("source and homepage must be https:// addresses".to_owned()));
        }
        if self.versions.is_empty() {
            return Err(CatalogError("no versions".to_owned()));
        }
        let mut seen = BTreeSet::new();
        for release in &self.versions {
            if !seen.insert(&release.version) {
                return Err(CatalogError(format!("version {} is listed more than once", release.version)));
            }
            release.validate().map_err(|err| CatalogError(format!("version {}: {err}", release.version)))?;
        }
        Ok(())
    }

    /// The name in `language`, or the English one.
    pub fn name(&self, language: &str) -> &str {
        localized(&self.name, language)
    }

    /// The description in `language`, or the English one.
    pub fn description(&self, language: &str) -> &str {
        localized(&self.description, language)
    }

    /// The newest version.
    pub fn newest(&self) -> Option<&Release> {
        self.versions.iter().max_by(|a, b| a.version.cmp(&b.version))
    }

    /// The newest version that works with the plugin API `api`. Pre-releases
    /// are offered only when there is nothing else.
    pub fn best_release(&self, api: &Version) -> Option<&Release> {
        let works = self.versions.iter().filter(|release| release.api.matches(api));
        let stable = works.clone().filter(|release| release.version.pre.is_empty());
        stable.max_by(|a, b| a.version.cmp(&b.version)).or_else(|| works.max_by(|a, b| a.version.cmp(&b.version)))
    }
}

impl Release {
    /// Checks what the types alone don't.
    pub fn validate(&self) -> Result<(), CatalogError> {
        let relative = !self.url.contains("://") && !self.url.starts_with('/') && !self.url.is_empty();
        if !self.url.starts_with("https://") && !relative {
            return Err(CatalogError(
                "url must be an https:// address or a path relative to the catalog file".to_owned(),
            ));
        }
        let hex = self.sha256.len() == 64 && self.sha256.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'));
        if !hex {
            return Err(CatalogError("sha256 must be 64 lower-case hex digits".to_owned()));
        }
        if !(1..=MAX_ARCHIVE_SIZE).contains(&self.size) {
            return Err(CatalogError(format!("size must be from 1 to {MAX_ARCHIVE_SIZE} bytes")));
        }
        Ok(())
    }

    /// Checks a downloaded archive: its size and SHA-256 (`sha256`, already
    /// computed by the caller, in hex).
    pub fn check_archive(&self, size: u64, sha256: &str) -> Result<(), CatalogError> {
        if size != self.size {
            return Err(CatalogError(format!("the archive has {size} bytes, the catalog says {}", self.size)));
        }
        if !sha256.eq_ignore_ascii_case(&self.sha256) {
            return Err(CatalogError("the archive's SHA-256 is not the one in the catalog".to_owned()));
        }
        Ok(())
    }

    /// Checks that the archive's manifest is the plugin and version the
    /// catalog lists.
    pub fn check_manifest(&self, id: &str, manifest: &Manifest) -> Result<(), CatalogError> {
        if manifest.id != id {
            return Err(CatalogError(format!("the archive holds \"{}\", not \"{id}\"", manifest.id)));
        }
        if manifest.version != self.version {
            return Err(CatalogError(format!("the archive holds version {}, not {}", manifest.version, self.version)));
        }
        if manifest.api != self.api {
            return Err(CatalogError(format!("the archive needs API {}, the catalog says {}", manifest.api, self.api)));
        }
        Ok(())
    }
}

fn localized<'a>(texts: &'a BTreeMap<String, String>, language: &str) -> &'a str {
    texts.get(language).or_else(|| texts.get("en")).map_or("", String::as_str)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn release(version: &str, api: &str) -> serde_json::Value {
        json!({ "version": version, "api": api, "url": format!("https://example.com/{version}.zip"),
                "sha256": HASH, "size": 1000 })
    }

    fn plugin(id: &str, versions: Vec<serde_json::Value>) -> serde_json::Value {
        json!({
            "id": id,
            "name": { "en": "Photo info", "ru": "Сведения о фото" },
            "description": { "en": "Shows what a photo is." },
            "author": "RustCloud", "license": "MIT", "source": "https://github.com/RustCloud/photo-info",
            "versions": versions,
        })
    }

    fn catalog(plugins: Vec<serde_json::Value>) -> Catalog {
        Catalog::from_json(&json!({ "schema": 1, "plugins": plugins }).to_string()).expect("readable")
    }

    #[test]
    fn reads_a_catalog_and_ignores_unknown_fields() {
        let mut entry = plugin("photo-info", vec![release("0.1.0", "^0.2.16")]);
        entry["tags"] = json!(["photo"]);
        let catalog = catalog(vec![entry]);
        assert_eq!(catalog.skipped, Vec::<String>::new());
        let entry = &catalog.plugins[0];
        assert_eq!((entry.name("ru"), entry.name("de")), ("Сведения о фото", "Photo info"));
        assert_eq!(entry.description("ru"), "Shows what a photo is.");
    }

    #[test]
    fn skips_broken_and_repeated_entries_only() {
        let mut no_english = plugin("b", vec![release("0.1.0", "^0.2")]);
        no_english["name"] = json!({ "ru": "Б" });
        let catalog = catalog(vec![
            plugin("a", vec![release("0.1.0", "^0.2")]),
            no_english,
            plugin("Bad Id", vec![release("0.1.0", "^0.2")]),
            plugin("c", vec![]),
            plugin("d", vec![release("0.1.0", "^0.2"), release("0.1.0", "^0.2")]),
            json!({ "id": "e" }),
            json!(42),
            plugin("a", vec![release("0.2.0", "^0.2")]),
        ]);
        let ids: Vec<_> = catalog.plugins.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["a"]);
        assert_eq!(catalog.skipped.len(), 7, "{:?}", catalog.skipped);
        assert!(catalog.skipped[0].starts_with("\"b\": name.en"), "{}", catalog.skipped[0]);
        assert!(catalog.skipped[5].starts_with("entry 7: "), "{}", catalog.skipped[5]);
        assert_eq!(catalog.skipped[6], "\"a\": listed more than once");
    }

    #[test]
    fn the_documented_example_is_valid() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let docs = std::fs::read_to_string(root.join("../../docs/dev/plugin-catalog.md")).unwrap();
        let example = docs.split("```json\n").nth(1).and_then(|rest| rest.split("```").next()).unwrap();
        let catalog = Catalog::from_json(example).expect("readable");
        assert_eq!((catalog.plugins.len(), catalog.skipped.len()), (1, 0), "{:?}", catalog.skipped);
        assert_eq!(catalog.plugins[0].best_release(&crate::api_version()).map(|r| r.size), Some(113899));
    }

    #[test]
    fn rejects_other_formats() {
        let err = Catalog::from_json(r#"{"schema": 2, "plugins": []}"#).unwrap_err();
        assert!(err.0.contains("format 2"), "{err}");
        assert!(Catalog::from_json("[]").is_err());
    }

    #[test]
    fn checks_releases() {
        let valid: Release = serde_json::from_value(release("0.1.0", "^0.2")).unwrap();
        assert_eq!(valid.validate(), Ok(()));
        let with = |change: &dyn Fn(&mut Release)| {
            let mut release = valid.clone();
            change(&mut release);
            release.validate()
        };
        assert_eq!(with(&|r| r.url = "plugins/a-0.1.0.zip".into()), Ok(()), "relative to the catalog");
        assert!(with(&|r| r.url = "http://example.com/a.zip".into()).is_err());
        assert!(with(&|r| r.url = "/a.zip".into()).is_err());
        assert!(with(&|r| r.url = "file:///etc/passwd".into()).is_err());
        assert!(with(&|r| r.sha256 = HASH.to_uppercase()).is_err());
        assert!(with(&|r| r.sha256 = "abc".into()).is_err());
        assert!(with(&|r| r.size = 0).is_err());
        assert!(with(&|r| r.size = MAX_ARCHIVE_SIZE + 1).is_err());
    }

    #[test]
    fn offers_the_newest_version_that_works_here() {
        let listed = catalog(vec![plugin(
            "a",
            vec![
                release("0.1.0", "^0.2.0"),
                release("0.3.0", "^0.2.10"),
                release("0.4.0-beta.1", "^0.2.10"),
                release("1.0.0", "^0.3"),
            ],
        )]);
        let entry = &listed.plugins[0];
        let best = |api: &str| entry.best_release(&Version::parse(api).unwrap()).map(|r| r.version.to_string());
        assert_eq!(best("0.2.16").as_deref(), Some("0.3.0"), "stable before a pre-release");
        assert_eq!(best("0.2.5").as_deref(), Some("0.1.0"));
        assert_eq!(best("0.3.1").as_deref(), Some("1.0.0"));
        assert_eq!(best("0.1.0"), None);
        assert_eq!(entry.newest().map(|r| r.version.to_string()).as_deref(), Some("1.0.0"));

        let beta_only = catalog(vec![plugin("a", vec![release("0.1.0-beta.1", "^0.2")])]);
        assert_eq!(
            beta_only.plugins[0].best_release(&Version::new(0, 2, 16)).map(|r| r.version.to_string()).as_deref(),
            Some("0.1.0-beta.1"),
            "a pre-release when there is nothing else"
        );
    }

    #[test]
    fn checks_the_downloaded_archive() {
        let release: Release = serde_json::from_value(release("0.1.0", "^0.2.16")).unwrap();
        assert_eq!(release.check_archive(1000, &HASH.to_uppercase()), Ok(()));
        assert!(release.check_archive(999, HASH).is_err());
        assert!(release.check_archive(1000, &HASH.replace('0', "1")).is_err());

        let manifest = |id: &str, version: &str, api: &str| {
            Manifest::from_toml(&format!(
                "id = \"{id}\"\nversion = \"{version}\"\napi = \"{api}\"\n[name]\nen = \"A\"\n"
            ))
            .unwrap()
        };
        assert_eq!(release.check_manifest("a", &manifest("a", "0.1.0", "^0.2.16")), Ok(()));
        assert!(release.check_manifest("a", &manifest("b", "0.1.0", "^0.2.16")).is_err());
        assert!(release.check_manifest("a", &manifest("a", "0.1.1", "^0.2.16")).is_err());
        assert!(release.check_manifest("a", &manifest("a", "0.1.0", "^0.2")).is_err());
    }
}
