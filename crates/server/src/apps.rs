//! The running apps (plugins) of this server, as `GET /api/v1/apps` lists
//! them: clients build their navigation from this list instead of knowing
//! the apps in advance. Which apps run is decided in `plugins.rs`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rustcloud_plugin_api::{api_version, Manifest, ManifestError, Version};
use serde::Serialize;

/// The apps that run on this server.
#[derive(Debug, Clone, Serialize)]
pub struct Apps {
    /// Version of the plugin API the server offers.
    pub api_version: Version,
    pub apps: Vec<Manifest>,
}

impl Apps {
    /// Checks the manifests and keeps them in the given order (the order of
    /// the side panel). An invalid manifest, a duplicate id or an app made for
    /// another API version is refused, naming the app and the reason.
    pub fn new(apps: Vec<Manifest>) -> Result<Self, ManifestError> {
        let mut list = Self { api_version: api_version(), apps: Vec::new() };
        for app in apps {
            list.add(app)?;
        }
        Ok(list)
    }

    /// The app that serves `/.well-known/<name>`, and its path for it.
    pub fn well_known(&self, name: &str) -> Option<(&str, &str)> {
        self.apps.iter().find_map(|app| Some((app.id.as_str(), app.well_known.get(name)?.as_str())))
    }

    /// The app with this id.
    pub fn get(&self, id: &str) -> Option<&Manifest> {
        self.apps.iter().find(|app| app.id == id)
    }

    pub(crate) fn add(&mut self, app: Manifest) -> Result<(), ManifestError> {
        self.check(&app)?;
        self.apps.push(app);
        Ok(())
    }

    /// Whether `app` can join the list: valid, a new id, and made for this API.
    fn check(&self, app: &Manifest) -> Result<(), ManifestError> {
        app.validate()?;
        let ids: BTreeSet<&str> = self.apps.iter().map(|a| a.id.as_str()).collect();
        if ids.contains(app.id.as_str()) {
            return Err(ManifestError(format!("two apps have the id \"{}\"", app.id)));
        }
        for name in app.well_known.keys() {
            if let Some(owner) = self.well_known(name) {
                return Err(ManifestError(format!(
                    "app \"{}\" asks for /.well-known/{name}, which app \"{}\" already has",
                    app.id, owner.0
                )));
            }
        }
        for proxy in &app.proxy {
            let taken = self.apps.iter().find(|other| other.proxy.iter().any(|theirs| theirs.overlaps(proxy)));
            if let Some(owner) = taken {
                return Err(ManifestError(format!(
                    "app \"{}\" asks for the paths {}, which app \"{}\" already has",
                    app.id,
                    proxy.paths.join(", "),
                    owner.id
                )));
            }
        }
        if !app.supports(&self.api_version) {
            return Err(ManifestError(format!(
                "app \"{}\" needs plugin API {}, this server has {}",
                app.id, app.api, self.api_version
            )));
        }
        Ok(())
    }
}

/// The subfolders of `dir`, sorted; none if it doesn't exist. Hidden ones
/// (an install in progress) are left out.
pub(crate) fn app_folders(dir: &Path) -> Vec<PathBuf> {
    let hidden = |path: &PathBuf| path.file_name().is_some_and(|name| name.to_string_lossy().starts_with('.'));
    let mut folders: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(entries) => entries.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_dir() && !hidden(p)).collect(),
        Err(_) => Vec::new(),
    };
    folders.sort();
    folders
}

#[cfg(test)]
mod tests {
    use rustcloud_plugin_api::VersionReq;

    use super::*;

    fn app(id: &str, api: &str) -> Manifest {
        Manifest {
            id: id.to_owned(),
            version: Version::new(1, 0, 0),
            api: VersionReq::parse(api).expect("valid requirement"),
            name: [("en".to_owned(), id.to_owned())].into(),
            provides: Default::default(),
            permissions: Default::default(),
            ui: None,
            well_known: Default::default(),
            server_settings: Vec::new(),
            limits: Default::default(),
            downloads: Vec::new(),
            jobs: Vec::new(),
            open_with: Vec::new(),
            large_bodies: false,
            new_files: Vec::new(),
            status: None,
            proxy: Vec::new(),
        }
    }

    #[test]
    fn duplicates_and_other_api_versions_are_refused() {
        let current = format!("={}", api_version());
        let err = Apps::new(vec![app("notes", &current), app("notes", &current)]).expect_err("duplicate");
        assert!(err.0.contains("two apps"), "{err}");

        let err = Apps::new(vec![app("notes", ">=99")]).expect_err("incompatible");
        assert!(err.0.contains("needs plugin API >=99"), "{err}");
    }

    #[test]
    fn a_discovery_address_belongs_to_one_app() {
        let current = format!("={}", api_version());
        let mut calendar = app("calendar", &current);
        calendar.well_known.insert("caldav".into(), "/dav/".into());
        let mut other = app("other-calendar", &current);
        other.well_known.insert("caldav".into(), "/".into());

        let err = Apps::new(vec![calendar.clone(), other]).expect_err("taken");
        assert_eq!(err.0, "app \"other-calendar\" asks for /.well-known/caldav, which app \"calendar\" already has");
        let apps = Apps::new(vec![calendar]).expect("one is fine");
        assert_eq!(apps.well_known("caldav"), Some(("calendar", "/dav/")));
        assert_eq!(apps.well_known("carddav"), None);
    }

    #[test]
    fn paths_of_a_proxy_belong_to_one_app() {
        let current = format!("={}", api_version());
        let with_proxy = |id: &str, paths: &[&str]| {
            let mut app = app(id, &current);
            let paths = paths.iter().map(|path| path.to_string()).collect();
            app.proxy.push(rustcloud_plugin_api::Proxy { target: "http://127.0.0.1:9980".into(), paths });
            app
        };
        let office = with_proxy("office", &["/browser", "/cool"]);
        let err = Apps::new(vec![office.clone(), with_proxy("other", &["/cool/ws"])]).expect_err("taken");
        assert_eq!(err.0, "app \"other\" asks for the paths /cool/ws, which app \"office\" already has");
        Apps::new(vec![office, with_proxy("other", &["/coolness"])]).expect("other paths are fine");
    }
}
