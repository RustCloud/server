//! The files app as the server and clients see it: its [`Manifest`].
//!
//! `files` is built into the server, but it is described like any other app,
//! so the web UI finds it the same way it will find WASM apps.

use rustcloud_plugin_api::{api_version, Manifest, Ui, Version, VersionReq};

/// Version of the `rustcloud:files` interface other apps use.
pub const FILES_INTERFACE_VERSION: Version = Version::new(0, 1, 0);

/// The manifest of the files app.
pub fn manifest() -> Manifest {
    let api = api_version();
    Manifest {
        id: "files".to_owned(),
        version: Version::parse(env!("CARGO_PKG_VERSION")).expect("crate version is semver"),
        // Built in, so always made for exactly this API.
        api: VersionReq::parse(&format!("={api}")).expect("valid requirement"),
        name: [("en", "Files"), ("ru", "Файлы")].map(|(lang, name)| (lang.to_owned(), name.to_owned())).into(),
        provides: [("rustcloud:files".to_owned(), FILES_INTERFACE_VERSION)].into(),
        permissions: Default::default(),
        ui: Some(Ui {
            icon: "folder".to_owned(),
            entry: "/files".to_owned(),
            routes: vec!["links".to_owned(), "search".to_owned(), "trash".to_owned()],
        }),
        well_known: Default::default(),
        server_settings: Vec::new(),
        downloads: Vec::new(),
        jobs: Vec::new(),
        open_with: Vec::new(),
        large_bodies: false,
        new_files: Vec::new(),
        status: None,
        proxy: Vec::new(),
        limits: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_manifest_is_valid_for_this_api() {
        let manifest = manifest();
        manifest.validate().expect("valid manifest");
        assert!(manifest.supports(&api_version()));
    }
}
