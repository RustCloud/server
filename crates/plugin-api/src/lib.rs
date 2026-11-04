//! The contract between RustCloud and its apps (plugins).
//!
//! Every app — built-in like `files` or loaded later from WASM — is described
//! by a [`Manifest`]: who it is, which versions of this API it works with,
//! what it offers to other apps and where it appears in the web UI. The server
//! and the clients learn about apps only from manifests (`GET /api/v1/apps`),
//! so a new app needs no changes in them.
//!
//! An app crate depends only on this crate, never on the server's internals:
//! that is what lets the same app later run as a WASM plugin.

pub mod limits;
mod manifest;
mod network;

pub use limits::{Limits, Scale};
pub use manifest::{
    proxy_path_matches, setting_key, valid_proxy_path, valid_storage_path, Download, FileScope, JobKind, Layout,
    Manifest, ManifestError, NewFile, OpenWith, Permissions, Proxy, ServerSetting, SettingKind, Ui, FILE_EVENTS,
    RESERVED_PATHS,
};
pub use network::{url_allowed, NetworkRule};
pub use semver::{Version, VersionReq};

/// Version of this API. Minor versions only add things; a major version may
/// break apps, which then declare it in [`Manifest::api`]. Before 1.0 a minor
/// version may break too (the semver rule for `0.x`).
pub const API_VERSION: &str = "0.2.16";

/// [`API_VERSION`] parsed.
pub fn api_version() -> Version {
    Version::parse(API_VERSION).expect("API_VERSION is a valid version")
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_plugin_template_has_the_current_contract() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let contract = std::fs::read_to_string(root.join("wit/app.wit")).unwrap();
        let template = std::fs::read_to_string(root.join("../../examples/plugin-template/wit/app.wit")).unwrap();
        assert!(template == contract, "copy wit/app.wit to examples/plugin-template/wit/");
        assert!(contract.contains(&format!("package rustcloud:app@{};", super::API_VERSION)));
    }
}
