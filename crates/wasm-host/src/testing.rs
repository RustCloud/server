//! Test helpers: WASM apps built for wasm32-wasip2 on first use — the echo
//! app (`tests/apps/echo`) and the apps shipped with the server (`apps/`).
//! Needs `rustup target add wasm32-wasip2`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use rustcloud_plugin_api::{Manifest, API_VERSION};

/// The workspace root.
fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Builds the app crate at `dir` (relative to the workspace) once per test
/// run and returns its component.
pub fn build_app(dir: &str, crate_name: &str) -> Vec<u8> {
    static BUILT: OnceLock<Mutex<HashMap<String, Vec<u8>>>> = OnceLock::new();
    let mut built = BUILT.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    built
        .entry(dir.to_owned())
        .or_insert_with(|| {
            let target_dir = workspace().join("target/test-apps");
            let status = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
                .args(["build", "--quiet", "--locked", "--release", "--target", "wasm32-wasip2", "--manifest-path"])
                .arg(workspace().join(dir).join("Cargo.toml"))
                .arg("--target-dir")
                .arg(&target_dir)
                .status()
                .expect("cargo runs");
            assert!(
                status.success(),
                "building {dir} failed; is the target installed? rustup target add wasm32-wasip2"
            );
            let wasm = target_dir.join(format!("wasm32-wasip2/release/{crate_name}.wasm"));
            std::fs::read(wasm).expect("the component was built")
        })
        .clone()
}

/// The echo app's component.
pub fn echo_wasm() -> Vec<u8> {
    build_app("crates/wasm-host/tests/apps/echo", "echo")
}

/// The echo app's manifest.
pub fn echo_manifest() -> Manifest {
    Manifest::from_toml(&echo_manifest_toml()).expect("valid manifest")
}

/// The echo app's `manifest.toml`.
pub fn echo_manifest_toml() -> String {
    format!(
        "id = \"echo\"\nversion = \"0.1.0\"\napi = \"={API_VERSION}\"\nlarge_bodies = true\n[name]\nen = \"Echo\"\n\
         [permissions]\nfiles = \"folder\"\nfiles_write = true\n"
    )
}

/// Puts the echo app into `apps_dir`, as the server reads apps.
pub fn install_echo(apps_dir: &Path) {
    install(apps_dir, "echo", &echo_manifest_toml(), &echo_wasm());
}

/// Puts an app shipped with the server (`apps/<id>`) into `apps_dir`.
pub fn install_shipped(apps_dir: &Path, id: &str) {
    let manifest = std::fs::read_to_string(workspace().join("apps").join(id).join("manifest.toml")).expect("manifest");
    install(apps_dir, id, &manifest, &build_app(&format!("apps/{id}"), id));
}

fn install(apps_dir: &Path, id: &str, manifest: &str, wasm: &[u8]) {
    let dir = apps_dir.join(id);
    std::fs::create_dir_all(&dir).expect("app folder");
    std::fs::write(dir.join("manifest.toml"), manifest).expect("manifest written");
    std::fs::write(dir.join("app.wasm"), wasm).expect("component written");
}
