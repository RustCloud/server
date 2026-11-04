//! [`Manifest`]: the description of an app.

use std::collections::BTreeMap;
use std::fmt;

use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};

use crate::limits::{self, Scale};
use crate::{url_allowed, NetworkRule};

/// What the server and the clients know about an app.
///
/// WASM apps ship it as `manifest.toml`:
///
/// ```toml
/// id = "notes"
/// version = "0.1.0"
/// api = "^0.1"
///
/// [name]
/// en = "Notes"
/// ru = "Заметки"
///
/// [ui]
/// icon = "note"
/// entry = "/app/notes"
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Unique id: lower-case letters, digits and `-`, e.g. `files`. Also the
    /// prefix of the app's routes, so it never changes.
    pub id: String,
    pub version: Version,
    /// Versions of the plugin API the app works with, e.g. `^0.1`.
    pub api: VersionReq,
    /// The app's name by language code; `en` is required and is the fallback.
    pub name: BTreeMap<String, String>,
    /// Interfaces the app offers to other apps, with their versions, e.g.
    /// `"rustcloud:files" = "0.1.0"`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub provides: BTreeMap<String, Version>,
    /// The most the app may ask users for. The administrator sees this when
    /// installing the app; users grant actual access within it.
    #[serde(default, skip_serializing_if = "Permissions::is_none")]
    pub permissions: Permissions,
    /// The app's place in the web UI; `None` for apps without pages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui: Option<Ui>,
    /// Discovery addresses of protocols the app serves, redirected to a path
    /// of its own: `caldav = "/dav/"` sends `/.well-known/caldav` to
    /// `/app/<id>/dav/`. One address belongs to one app.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub well_known: BTreeMap<String, String>,
    /// Settings of the whole server the administrator fills in on the
    /// Plugins tab (an API key, a model name), in this order. The app reads
    /// them through its `settings` import.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub server_settings: Vec<ServerSetting>,
    /// Files the server downloads into the app's storage once the app is
    /// enabled (a model, reference data), from addresses in
    /// `permissions.network`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub downloads: Vec<Download>,
    /// Kinds of background jobs the app does (`run-batch`), e.g. reading
    /// photo metadata or finding faces, each in batches of its own size.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub jobs: Vec<JobKind>,
    /// Files the app opens: the files panel offers it for these types, and
    /// the user's choice gives it access to the file while it is open
    /// (since 0.2.14).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_with: Vec<OpenWith>,
    /// Kinds of new files the files panel's "New" menu offers for the app,
    /// e.g. an office document (since 0.2.16). The panel creates the file in
    /// the folder being shown and opens it with the app's `[[open_with]]`
    /// for its type, if any.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub new_files: Vec<NewFile>,
    /// The app's path the administrator's Plugins tab reads to show how the
    /// app is doing, e.g. `/status`: "the model is missing", "the editor
    /// answers" (since 0.2.16). It answers a JSON array of
    /// `{"level": "ok" | "info" | "warning" | "error", "text": …}`, the text
    /// a string or by language (`{"en": …, "ru": …}`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Request bodies larger than the app takes at once are kept on disk
    /// instead of refused: the app gets the request without its body and
    /// with the header `x-rustcloud-body-size`, and saves the body into a file
    /// with `files.write-request-body` (since 0.2.14). An editor's server
    /// saving a big document (WOPI `PutFile`) needs it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub large_bodies: bool,
    /// Paths of the server's own address that go to a service next to it,
    /// e.g. an office editor, WebSocket included (since 0.2.15).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub proxy: Vec<Proxy>,
    /// What the app needs of the server, as multiples of the base
    /// ([`crate::limits`]); the administrator may give it more or less.
    #[serde(default, skip_serializing_if = "Scale::is_default")]
    pub limits: Scale,
}

/// A kind of background job:
///
/// ```toml
/// [[jobs]]
/// kind = "faces"
/// batch = 5           # jobs per run-batch call
/// time = 16           # the app's own work in one batch: 16 times a request's
/// memory = 8          # 8 times a request's memory
/// on = ["file.granted", "file.created", "file.changed"]
/// mime = ["image/*"]
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobKind {
    /// Lower-case letters, digits, `-` and `_`.
    pub kind: String,
    /// Jobs per batch, 1–1000.
    #[serde(default = "default_batch")]
    pub batch: u32,
    /// The app's own work in one batch, as a multiple of a request's
    /// ([`limits::SCALES`]); none: as a request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<u32>,
    /// Memory of one batch, as a multiple of a request's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<u32>,
    /// Before API 0.2.9: the same in seconds and MiB, read as the next step up.
    #[serde(default, skip_serializing)]
    pub time_seconds: Option<u32>,
    #[serde(default, skip_serializing)]
    pub memory_mib: Option<u32>,
    /// Events of the users' files that add a job of this kind by themselves,
    /// with the file's id as its key, in folders the user gave the app:
    /// [`FILE_EVENTS`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub on: Vec<String>,
    /// Only for files of these types: `image/jpeg`, `image/*`; none: all.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mime: Vec<String>,
}

/// A kind of files the app opens:
///
/// ```toml
/// [[open_with]]
/// mime = ["text/markdown", "text/plain"]
/// page = "/edit"      # the panel opens /app/<id>/edit?file=<id>
/// write = true        # changes the file too (needs files_write)
/// layout = "full"     # the whole browser window (default) or "panel"
/// label = { en = "Edit", ru = "Редактировать" }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenWith {
    /// Types like `text/markdown` or `image/*`.
    pub mime: Vec<String>,
    /// The app's page that shows the file, starting with `/`; the panel adds
    /// `?file=<id>`.
    pub page: String,
    /// Whether the app may change the file, not only read it.
    #[serde(default)]
    pub write: bool,
    #[serde(default)]
    pub layout: Layout,
    /// What the menu says, by language; "Open in <app>" if none.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub label: BTreeMap<String, String>,
}

/// A kind of new file (since 0.2.16):
///
/// ```toml
/// [[new_files]]
/// extension = "docx"
/// label = { en = "Document", ru = "Документ" }
/// template = "/templates/empty.docx"   # the app's path giving its content; none: empty
/// ```
///
/// The file is named after the label (`Document.docx`), the user may change
/// the name. The template is read with the user's session, like the app's
/// pages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewFile {
    /// 1–16 lower-case letters and digits, without the dot.
    pub extension: String,
    /// What the menu says and the file's first name, by language; `en` is required.
    pub label: BTreeMap<String, String>,
    /// The app's path whose answer is the new file's content, starting with
    /// `/`; none: the file starts empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
}

/// Paths of the server that go to a service the administrator runs next to
/// it (since 0.2.15):
///
/// ```toml
/// [[proxy]]
/// target = "{office_url}"              # an address, or a text server setting holding one
/// paths = ["/browser", "/cool"]        # at the root of the server's address
/// ```
///
/// The browser reaches the service through the server's address only, so
/// the service needs no address of its own. It runs on the server's origin:
/// approving the app means trusting the service as part of the server. Only
/// a user with a file open in the app (a session grant) reaches it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Proxy {
    /// `http://collabora:9980`, or `{key}`: a text server setting.
    pub target: String,
    /// Prefixes of paths, see [`valid_proxy_path`].
    pub paths: Vec<String>,
}

/// First names of the server's paths no [`Proxy`] takes: the server's own.
/// Files of the web UI have a `.` in their names, which proxy paths can't.
pub const RESERVED_PATHS: [&str; 4] = ["api", "app", "assets", "health"];

/// Whether `path` can be a prefix of a [`Proxy`]: `/` and 1–4 names of
/// lower-case letters, digits, `-` and `_` separated by `/`, the first not
/// one of [`RESERVED_PATHS`]: `/browser`, `/cool`, `/hosting/discovery`.
pub fn valid_proxy_path(path: &str) -> bool {
    let Some(rest) = path.strip_prefix('/') else { return false };
    let names: Vec<&str> = rest.split('/').collect();
    let valid_name = |name: &&str| {
        !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    };
    names.len() <= 4 && names.iter().all(valid_name) && !RESERVED_PATHS.contains(&names[0])
}

/// Whether a request for `path` goes to the proxy prefix `prefix`: the
/// prefix itself or below it, `/cool/x` but not `/coolness`.
pub fn proxy_path_matches(prefix: &str, path: &str) -> bool {
    path.strip_prefix(prefix).is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

impl Proxy {
    /// The service's origin (`http://collabora:9980`), `{key}` read from the
    /// server settings with `setting`; none while the setting is empty or invalid.
    pub fn target_origin(&self, setting: impl Fn(&str) -> Option<String>) -> Option<String> {
        resolve(std::slice::from_ref(&self.target), setting).first().map(NetworkRule::origin)
    }

    /// Whether this and `other` share a path: one prefix within the other.
    pub fn overlaps(&self, other: &Proxy) -> bool {
        self.paths.iter().any(|a| other.paths.iter().any(|b| proxy_path_matches(a, b) || proxy_path_matches(b, a)))
    }
}

/// Where an app's page is shown.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    /// The whole browser window, with only the panel's close button over it.
    #[default]
    Full,
    /// The page area of the panel, next to its side bar.
    Panel,
}

/// What a job kind may start [`on`](JobKind::on): a file that becomes the
/// app's when the user gives it a folder, a new file, a changed, deleted or
/// moved one.
pub const FILE_EVENTS: [&str; 5] = ["file.granted", "file.created", "file.changed", "file.deleted", "file.moved"];

impl JobKind {
    /// Whether `event` on a file of type `mime` adds a job of this kind.
    pub fn wants(&self, event: &str, mime: Option<&str>) -> bool {
        let type_fits = |pattern: &String| match (pattern.strip_suffix("/*"), mime) {
            (Some(prefix), Some(mime)) => mime.split('/').next() == Some(prefix),
            (None, Some(mime)) => pattern.as_str() == mime,
            (_, None) => false,
        };
        self.on.iter().any(|on| on == event) && (self.mime.is_empty() || self.mime.iter().any(type_fits))
    }

    /// Memory and time of one batch, as multiples of a request's.
    pub fn scale(&self) -> (u32, u32) {
        let base_mib = limits::BASE_MEMORY / (1024 * 1024);
        let base_seconds = limits::BASE_TIME.as_secs();
        let memory = self.memory.or(self.memory_mib.map(|mib| limits::step_up(u64::from(mib).div_ceil(base_mib))));
        let time = self.time.or(self.time_seconds.map(|s| limits::step_up(u64::from(s).div_ceil(base_seconds))));
        (memory.unwrap_or(1), time.unwrap_or(1))
    }
}

fn default_batch() -> u32 {
    10
}

/// A file the server downloads for an app:
///
/// ```toml
/// [[downloads]]
/// path = "models/faces.onnx"
/// url = "https://huggingface.co/…/faces.onnx"
/// sha256 = "…"
/// size = 174000000
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Download {
    /// Where it goes in the app's storage, see [`valid_storage_path`].
    pub path: String,
    pub url: String,
    /// SHA-256 of the file, hex: a file that doesn't match is thrown away.
    pub sha256: String,
    /// Bytes, if known: shown as the progress of the download.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

/// Whether `path` can name a file in an app's storage: names separated by
/// `/`, none empty, `.` or `..`, no `\` or control characters; up to 1024 bytes.
pub fn valid_storage_path(path: &str) -> bool {
    path.len() <= 1024
        && path.split('/').all(|name| {
            !name.is_empty() && name != "." && name != ".." && !name.chars().any(|c| c == '\\' || c.is_control())
        })
}

/// A setting of a plugin for the whole server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerSetting {
    /// Lower-case letters, digits and `_`, e.g. `api_key`.
    pub key: String,
    pub kind: SettingKind,
    /// The label by language code; `en` is required.
    pub label: BTreeMap<String, String>,
    /// A line under the field, by language code.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub hint: BTreeMap<String, String>,
    /// For numbers, e.g. `s` or `MiB`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}

/// How a server setting of a plugin is edited and stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingKind {
    Bool,
    /// A whole number.
    Number,
    /// A line of text.
    Text,
    /// Lines of text.
    List,
    /// Text that is never shown again once saved, e.g. an API key.
    Secret,
}

/// What an app may ask users for.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Permissions {
    /// Access to the users' files.
    #[serde(default)]
    pub files: FileScope,
    /// Whether that access may include changing files.
    #[serde(default)]
    pub files_write: bool,
    /// Addresses the app may send HTTP requests to, see [`NetworkRule`]:
    /// `api.example.com`, `*.example.com`, `http://192.168.1.10:11434`; or
    /// `{office_url}`: the address in the server setting `office_url`, which
    /// differs from server to server (since 0.2.14).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub network: Vec<String>,
    /// Services the panel may show for the app's page, outside its sandbox
    /// (an editor such as Collabora): addresses as in `network`, usually
    /// `{office_url}` (since 0.2.14).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frame: Vec<String>,
    /// Native tools of the server the app uses, see [`USES`]: `use = ["ffmpeg"]`.
    /// The administrator approves them, shown as `use.ffmpeg`.
    #[serde(default, rename = "use", skip_serializing_if = "Vec::is_empty")]
    pub uses: Vec<String>,
}

/// What an app may [`use`](Permissions::uses), each through an import of its
/// own (`use-ffmpeg`, `use-onnx`): heavy native code the server runs for
/// apps, confined.
pub const USES: [&str; 2] = ["ffmpeg", "onnx"];

impl Permissions {
    pub fn is_none(&self) -> bool {
        self.files == FileScope::None && self.network.is_empty() && self.uses.is_empty() && self.frame.is_empty()
    }

    /// The fixed rules of [`Permissions::network`]; ones from settings and
    /// ones that don't parse are left out.
    pub fn network_rules(&self) -> Vec<NetworkRule> {
        self.network.iter().filter_map(|rule| NetworkRule::parse(rule).ok()).collect()
    }

    /// The rules of [`Permissions::network`], `{key}` read from the server
    /// settings with `setting`; an empty or invalid setting gives none.
    pub fn network_rules_with(&self, setting: impl Fn(&str) -> Option<String>) -> Vec<NetworkRule> {
        resolve(&self.network, setting)
    }

    /// The origins of [`Permissions::frame`] (`https://office.example.com`),
    /// `{key}` read from the server settings with `setting`.
    pub fn frame_origins(&self, setting: impl Fn(&str) -> Option<String>) -> Vec<String> {
        resolve(&self.frame, setting).iter().map(NetworkRule::origin).collect()
    }
}

/// The key of an entry `{key}`: an address in a server setting.
pub fn setting_key(entry: &str) -> Option<&str> {
    entry.strip_prefix('{')?.strip_suffix('}')
}

fn resolve(entries: &[String], setting: impl Fn(&str) -> Option<String>) -> Vec<NetworkRule> {
    entries
        .iter()
        .filter_map(|entry| match setting_key(entry) {
            Some(key) => NetworkRule::from_url(setting(key)?.trim()).ok(),
            None => NetworkRule::parse(entry).ok(),
        })
        .collect()
}

/// How much of a user's files an app may ask for. Each level includes the ones
/// before it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileScope {
    /// No access to files.
    #[default]
    None,
    /// A file the user opens in the app, while it is open.
    Opened,
    /// Folders the user chooses, until the user revokes it.
    Folder,
    /// All of the user's files, until the user revokes it.
    All,
}

/// An app's entry in the side panel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ui {
    /// Icon name from the web UI's icon set; unknown names get a generic icon.
    pub icon: String,
    /// Where the panel link leads, e.g. `/files`.
    pub entry: String,
    /// First path segments that belong to the app, so its link stays
    /// highlighted on them. The entry's own segment is always included.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<String>,
}

/// A manifest that can't be used, with the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestError(pub String);

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ManifestError {}

impl Manifest {
    /// Reads and checks a `manifest.toml`.
    pub fn from_toml(text: &str) -> Result<Self, ManifestError> {
        let manifest: Self = toml::from_str(text).map_err(|err| ManifestError(format!("invalid manifest: {err}")))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Checks what the types alone don't.
    pub fn validate(&self) -> Result<(), ManifestError> {
        let valid_id = !self.id.is_empty()
            && self.id.len() <= 40
            && !self.id.starts_with('-')
            && self.id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if !valid_id {
            return Err(ManifestError(format!(
                "app id \"{}\" must be 1–40 lower-case letters, digits or \"-\"",
                self.id
            )));
        }
        if !self.name.contains_key("en") {
            return Err(ManifestError(format!("app \"{}\" has no English name (name.en)", self.id)));
        }
        if let Some(ui) = &self.ui {
            if !ui.entry.starts_with('/') {
                return Err(ManifestError(format!("app \"{}\": ui.entry must start with \"/\"", self.id)));
            }
        }
        let mut keys = std::collections::BTreeSet::new();
        for setting in &self.server_settings {
            let valid_key = !setting.key.is_empty()
                && setting.key.len() <= 40
                && setting.key.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
            if !valid_key || !keys.insert(setting.key.as_str()) || !setting.label.contains_key("en") {
                return Err(ManifestError(format!(
                    "app \"{}\": server setting \"{}\" needs a unique lower-case key and an English label",
                    self.id, setting.key
                )));
            }
        }
        if self.server_settings.len() > 50 {
            return Err(ManifestError(format!("app \"{}\" has more than 50 server settings", self.id)));
        }
        let text_settings: Vec<&str> =
            self.server_settings.iter().filter(|s| s.kind == SettingKind::Text).map(|s| s.key.as_str()).collect();
        for (field, entries) in [("network", &self.permissions.network), ("frame", &self.permissions.frame)] {
            for rule in entries {
                let checked = match setting_key(rule) {
                    Some(key) if text_settings.contains(&key) => Ok(()),
                    Some(key) => Err(format!("{{{key}}} must name a text server setting")),
                    None => NetworkRule::parse(rule).map(|_| ()),
                };
                checked.map_err(|reason| {
                    ManifestError(format!("app \"{}\": permissions.{field} \"{rule}\": {reason}", self.id))
                })?;
            }
        }
        if self.permissions.frame.len() > 10 {
            return Err(ManifestError(format!("app \"{}\" asks for more than 10 frame addresses", self.id)));
        }
        let mut uses = std::collections::BTreeSet::new();
        for tool in &self.permissions.uses {
            if !USES.contains(&tool.as_str()) || !uses.insert(tool.as_str()) {
                return Err(ManifestError(format!(
                    "app \"{}\": permissions.use \"{tool}\" is not one of {}",
                    self.id,
                    USES.join(", ")
                )));
            }
        }
        let rules = self.permissions.network_rules();
        let mut paths = std::collections::BTreeSet::new();
        for download in &self.downloads {
            let problem = if !valid_storage_path(&download.path) || !paths.insert(download.path.as_str()) {
                Some("needs a unique path of names separated by \"/\"")
            } else if download.sha256.len() != 64 || !download.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
                Some("needs the file's SHA-256 in hex")
            } else if !url_allowed(&rules, &download.url) {
                Some("must come from an address in permissions.network")
            } else {
                None
            };
            if let Some(problem) = problem {
                return Err(ManifestError(format!("app \"{}\": download \"{}\" {problem}", self.id, download.path)));
            }
        }
        let mut kinds = std::collections::BTreeSet::new();
        for job in &self.jobs {
            let valid_kind = !job.kind.is_empty()
                && job.kind.len() <= 40
                && job.kind.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
            let problem = if !valid_kind || !kinds.insert(job.kind.as_str()) {
                Some("needs a unique kind of lower-case letters, digits, \"-\" or \"_\"")
            } else if !(1..=1000).contains(&job.batch) {
                Some("batch must be from 1 to 1000")
            } else if [job.time, job.memory].into_iter().flatten().any(|step| !limits::SCALES.contains(&step)) {
                Some("time and memory must be one of 1, 2, 4, 8, 16, 32, 64")
            } else if job.time_seconds.is_some_and(|s| !(1..=3600).contains(&s)) {
                Some("time_seconds must be from 1 to 3600")
            } else if job.memory_mib.is_some_and(|mib| !(16..=4096).contains(&mib)) {
                Some("memory_mib must be from 16 to 4096")
            } else if job.on.iter().any(|event| !FILE_EVENTS.contains(&event.as_str())) {
                Some("may be started on file.granted, file.created, file.changed, file.deleted or file.moved")
            } else if !job.on.is_empty() && self.permissions.files < FileScope::Folder {
                Some("can start on file events only with folder access (permissions.files)")
            } else if !valid_mime(&job.mime) {
                Some("mime must be types like image/jpeg or image/*")
            } else {
                None
            };
            if let Some(problem) = problem {
                return Err(ManifestError(format!("app \"{}\": job \"{}\" {problem}", self.id, job.kind)));
            }
        }
        for (i, open) in self.open_with.iter().enumerate() {
            let problem = if open.mime.is_empty() || !valid_mime(&open.mime) {
                Some("needs mime types like text/markdown or image/*")
            } else if !open.page.starts_with('/') {
                Some("page must start with \"/\"")
            } else if self.permissions.files < FileScope::Opened {
                Some("needs access to opened files (permissions.files)")
            } else if open.write && !self.permissions.files_write {
                Some("may change files only with permissions.files_write")
            } else {
                None
            };
            if let Some(problem) = problem {
                return Err(ManifestError(format!("app \"{}\": open_with #{} {problem}", self.id, i + 1)));
            }
        }
        let mut extensions = std::collections::BTreeSet::new();
        for (i, new_file) in self.new_files.iter().enumerate() {
            let valid_extension = !new_file.extension.is_empty()
                && new_file.extension.len() <= 16
                && new_file.extension.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
            let problem = if !valid_extension || !extensions.insert(new_file.extension.as_str()) {
                Some("needs a unique extension of 1–16 lower-case letters and digits")
            } else if !new_file.label.contains_key("en") {
                Some("needs an English label (label.en)")
            } else if new_file.template.as_ref().is_some_and(|path| !path.starts_with('/')) {
                Some("template must start with \"/\"")
            } else {
                None
            };
            if let Some(problem) = problem {
                return Err(ManifestError(format!("app \"{}\": new_files #{} {problem}", self.id, i + 1)));
            }
        }
        if self.new_files.len() > 20 {
            return Err(ManifestError(format!("app \"{}\" has more than 20 new_files entries", self.id)));
        }
        if self.status.as_ref().is_some_and(|path| !path.starts_with('/')) {
            return Err(ManifestError(format!("app \"{}\": status must start with \"/\"", self.id)));
        }
        if self.open_with.len() > 20 {
            return Err(ManifestError(format!("app \"{}\" has more than 20 open_with entries", self.id)));
        }
        if let Some(problem) = self.limits.problem() {
            return Err(ManifestError(format!("app \"{}\": limits: {problem}", self.id)));
        }
        if self.jobs.len() > 20 {
            return Err(ManifestError(format!("app \"{}\" has more than 20 kinds of jobs", self.id)));
        }
        if self.downloads.len() > 20 {
            return Err(ManifestError(format!("app \"{}\" has more than 20 downloads", self.id)));
        }
        for (i, proxy) in self.proxy.iter().enumerate() {
            let target = match setting_key(&proxy.target) {
                Some(key) if text_settings.contains(&key) => Ok(()),
                Some(key) => Err(format!("{{{key}}} must name a text server setting")),
                None => NetworkRule::from_url(&proxy.target).map(|_| ()),
            };
            let others = self.proxy.iter().enumerate().filter(|(j, _)| *j != i);
            let problem = if let Err(reason) = target {
                Some(format!("target \"{}\": {reason}", proxy.target))
            } else if proxy.paths.is_empty() || proxy.paths.len() > 8 {
                Some("needs 1–8 paths".to_owned())
            } else if let Some(path) = proxy.paths.iter().find(|path| !valid_proxy_path(path)) {
                Some(format!(
                    "path \"{path}\" must be \"/\" and up to 4 lower-case names, not starting with {}",
                    RESERVED_PATHS.map(|name| format!("/{name}")).join(", ")
                ))
            } else if others.clone().any(|(_, other)| proxy.overlaps(other))
                || proxy.paths.iter().enumerate().any(|(n, a)| {
                    proxy.paths[n + 1..].iter().any(|b| proxy_path_matches(a, b) || proxy_path_matches(b, a))
                })
            {
                Some("has paths within each other".to_owned())
            } else {
                None
            };
            if let Some(problem) = problem {
                return Err(ManifestError(format!("app \"{}\": proxy #{} {problem}", self.id, i + 1)));
            }
        }
        if self.proxy.len() > 4 {
            return Err(ManifestError(format!("app \"{}\" has more than 4 proxies", self.id)));
        }
        if self.permissions.network.len() > 20 {
            return Err(ManifestError(format!("app \"{}\" asks for more than 20 network addresses", self.id)));
        }
        for (name, path) in &self.well_known {
            let valid_name =
                !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
            if !valid_name || !path.starts_with('/') {
                return Err(ManifestError(format!(
                    "app \"{}\": well_known.{name} must be a lower-case name and a path starting with \"/\"",
                    self.id
                )));
            }
        }
        Ok(())
    }

    /// Whether the app works with the server's plugin API `api`.
    pub fn supports(&self, api: &Version) -> bool {
        self.api.matches(api)
    }
}

/// Whether all are types like `image/jpeg` or `image/*`, in lower case.
fn valid_mime(types: &[String]) -> bool {
    types.iter().all(|m| m.split('/').count() == 2 && !m.chars().any(char::is_uppercase))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTES: &str = r#"
        id = "notes"
        version = "0.1.0"
        api = "^0.1"

        [name]
        en = "Notes"
        ru = "Заметки"

        [provides]
        "rustcloud:notes" = "0.1.0"

        [permissions]
        files = "folder"
        files_write = true

        [ui]
        icon = "note"
        entry = "/app/notes"
    "#;

    #[test]
    fn a_manifest_is_read_from_toml() {
        let manifest = Manifest::from_toml(NOTES).expect("valid manifest");
        assert_eq!(manifest.id, "notes");
        assert_eq!(manifest.name["ru"], "Заметки");
        assert_eq!(manifest.provides["rustcloud:notes"], Version::new(0, 1, 0));
        assert_eq!(manifest.permissions.files, FileScope::Folder);
        assert!(manifest.permissions.files_write);
        assert!(manifest.permissions.network.is_empty());
        let media = Manifest::from_toml(&NOTES.replace("files_write = true", "use = [\"ffmpeg\", \"onnx\"]")).unwrap();
        assert_eq!(media.permissions.uses, ["ffmpeg", "onnx"]);
        assert!(FileScope::Folder > FileScope::Opened, "folder access includes opened files");
        assert_eq!(manifest.ui.expect("has ui").entry, "/app/notes");
    }

    #[test]
    fn server_settings_keep_their_order() {
        let text = format!(
            "{NOTES}\n[[server_settings]]\nkey = \"model\"\nkind = \"text\"\nlabel.en = \"Model\"\n\
             [[server_settings]]\nkey = \"api_key\"\nkind = \"secret\"\nlabel = {{ en = \"API key\", ru = \"Ключ API\" }}\n"
        );
        let manifest = Manifest::from_toml(&text).expect("valid manifest");
        let keys: Vec<&str> = manifest.server_settings.iter().map(|s| s.key.as_str()).collect();
        assert_eq!(keys, ["model", "api_key"]);
        assert_eq!(manifest.server_settings[1].kind, SettingKind::Secret);
    }

    #[test]
    fn downloads_come_from_the_apps_network_addresses() {
        let sha = "ab".repeat(32);
        let online = NOTES.replace("files_write = true", "files_write = true\nnetwork = [\"*.huggingface.co\"]");
        let download = |path: &str, url: &str, sha: &str| {
            format!("{online}\n[[downloads]]\npath = \"{path}\"\nurl = \"{url}\"\nsha256 = \"{sha}\"\n")
        };
        let manifest = Manifest::from_toml(&download("models/faces.onnx", "https://cdn.huggingface.co/f", &sha))
            .expect("valid manifest");
        assert_eq!(manifest.downloads[0].path, "models/faces.onnx");

        for (text, reason) in [
            (download("models/faces.onnx", "https://example.com/f", &sha), "permissions.network"),
            (download("../faces.onnx", "https://cdn.huggingface.co/f", &sha), "unique path"),
            (download("faces.onnx", "https://cdn.huggingface.co/f", "abc"), "SHA-256"),
        ] {
            let err = Manifest::from_toml(&text).expect_err("rejected");
            assert!(err.0.contains(reason), "{err} should mention {reason}");
        }
        assert!(valid_storage_path("a/.b/c.bin"));
        assert!(!valid_storage_path("") && !valid_storage_path("a//b") && !valid_storage_path("a/./b"));
    }

    #[test]
    fn job_kinds_have_their_batch_sizes() {
        let text =
            format!("{NOTES}\n[[jobs]]\nkind = \"exif\"\nbatch = 50\n[[jobs]]\nkind = \"faces\"\nmemory_mib = 512\n");
        let manifest = Manifest::from_toml(&text).expect("valid manifest");
        assert_eq!((manifest.jobs[0].batch, manifest.jobs[1].batch), (50, 10), "10 unless said otherwise");
        assert_eq!(manifest.jobs[1].memory_mib, Some(512));

        let photos = JobKind {
            on: vec!["file.created".into()],
            mime: vec!["image/*".into(), "video/mp4".into()],
            ..manifest.jobs[0].clone()
        };
        assert!(photos.wants("file.created", Some("image/jpeg")));
        assert!(photos.wants("file.created", Some("video/mp4")));
        assert!(!photos.wants("file.created", Some("video/webm")));
        assert!(!photos.wants("file.created", None), "no type, no match");
        assert!(!photos.wants("file.changed", Some("image/jpeg")), "only the events it names");
        assert!(JobKind { mime: Vec::new(), ..photos }.wants("file.created", None), "no types: all files");

        for (text, reason) in [
            (format!("{NOTES}\n[[jobs]]\nkind = \"Faces\"\n"), "unique kind"),
            (format!("{NOTES}\n[[jobs]]\nkind = \"a\"\n[[jobs]]\nkind = \"a\"\n"), "unique kind"),
            (format!("{NOTES}\n[[jobs]]\nkind = \"a\"\nbatch = 0\n"), "batch"),
            (format!("{NOTES}\n[[jobs]]\nkind = \"a\"\ntime_seconds = 4000\n"), "time_seconds"),
            (format!("{NOTES}\n[[jobs]]\nkind = \"a\"\non = [\"file.opened\"]\n"), "file.granted"),
            (format!("{NOTES}\n[[jobs]]\nkind = \"a\"\nmime = [\"image\"]\n"), "mime"),
            (
                format!(
                    "{}\n[[jobs]]\nkind = \"a\"\non = [\"file.created\"]\n",
                    NOTES.replace("\"folder\"", "\"opened\"")
                ),
                "folder access",
            ),
        ] {
            let err = Manifest::from_toml(&text).expect_err("rejected");
            assert!(err.0.contains(reason), "{err} should mention {reason}");
        }
    }

    #[test]
    fn api_versions_follow_semver() {
        let manifest = Manifest::from_toml(NOTES).expect("valid manifest");
        assert!(manifest.supports(&Version::new(0, 1, 5)));
        assert!(!manifest.supports(&Version::new(0, 2, 0)), "0.x minor versions may break");
        assert!(!manifest.supports(&Version::new(1, 0, 0)));
    }

    #[test]
    fn bad_manifests_are_rejected_with_a_reason() {
        let bad = [
            (NOTES.replace("\"notes\"", "\"My Notes\""), "lower-case"),
            (NOTES.replace("en = \"Notes\"", ""), "English name"),
            (NOTES.replace("\"/app/notes\"", "\"app/notes\""), "ui.entry"),
            (NOTES.replace("api = \"^0.1\"", "api = \"latest\""), "invalid manifest"),
            (format!("{NOTES}\nunknown = 1"), "unknown field"),
            (NOTES.replace("files = \"folder\"", "files = \"everything\""), "unknown variant"),
            (format!("{NOTES}\n[well_known]\ncaldav = \"dav\""), "well_known.caldav"),
            (NOTES.replace("files_write = true", "network = [\"example.com/api\"]"), "permissions.network"),
            (NOTES.replace("files_write = true", "use = [\"ffmpeg\", \"shell\"]"), "permissions.use"),
            (
                format!("{NOTES}\n[[server_settings]]\nkey = \"API key\"\nkind = \"secret\"\nlabel.en = \"Key\""),
                "server setting",
            ),
            (
                format!("{NOTES}\n[[server_settings]]\nkey = \"key\"\nkind = \"secret\"\nlabel.ru = \"Ключ\""),
                "English label",
            ),
        ];
        for (text, reason) in bad {
            let err = Manifest::from_toml(&text).expect_err("rejected");
            assert!(err.0.contains(reason), "{err} should mention {reason}");
        }
    }

    #[test]
    fn apps_open_the_files_they_declare_within_their_permissions() {
        let with = |entry: &str| format!("{NOTES}\n[[open_with]]\n{entry}\n");
        let manifest =
            Manifest::from_toml(&with("mime = [\"text/markdown\"]\npage = \"/edit\"\nwrite = true")).unwrap();
        let open = &manifest.open_with[0];
        assert_eq!((open.page.as_str(), open.write, open.layout), ("/edit", true, Layout::Full));
        let panel = Manifest::from_toml(&with("mime = [\"image/*\"]\npage = \"/view\"\nlayout = \"panel\"")).unwrap();
        assert_eq!(panel.open_with[0].layout, Layout::Panel);

        for (entry, reason) in [
            ("mime = []\npage = \"/edit\"", "needs mime types"),
            ("mime = [\"Text/Plain\"]\npage = \"/edit\"", "needs mime types"),
            ("mime = [\"text/plain\"]\npage = \"edit\"", "must start with"),
        ] {
            let err = Manifest::from_toml(&with(entry)).expect_err("rejected");
            assert!(err.0.contains(reason), "{err} should mention {reason}");
        }
        let read_only = with("mime = [\"text/plain\"]\npage = \"/e\"\nwrite = true").replace("files_write = true", "");
        assert!(Manifest::from_toml(&read_only).unwrap_err().0.contains("files_write"));
        let no_files = with("mime = [\"text/plain\"]\npage = \"/e\"").replace("files = \"folder\"", "files = \"none\"");
        assert!(Manifest::from_toml(&no_files).unwrap_err().0.contains("opened files"));
    }

    #[test]
    fn apps_offer_new_files_and_tell_their_status() {
        let text = format!(
            "status = \"/status\"\n{NOTES}\n[[new_files]]\nextension = \"docx\"\nlabel.en = \"Document\"\n\
             template = \"/templates/empty.docx\"\n[[new_files]]\nextension = \"md\"\nlabel.en = \"Note\"\n"
        );
        let manifest = Manifest::from_toml(&text).unwrap();
        assert_eq!(manifest.status.as_deref(), Some("/status"));
        assert_eq!(manifest.new_files[0].template.as_deref(), Some("/templates/empty.docx"));
        assert_eq!(manifest.new_files[1].template, None, "starts empty");

        for (from, to, reason) in [
            ("\"docx\"", "\".docx\"", "unique extension"),
            ("\"md\"", "\"docx\"", "unique extension"),
            ("label.en = \"Note\"", "label.ru = \"Заметка\"", "English label"),
            ("\"/templates/empty.docx\"", "\"empty.docx\"", "template must start"),
            ("\"/status\"", "\"status\"", "status must start"),
        ] {
            let err = Manifest::from_toml(&text.replacen(from, to, 1)).expect_err("rejected");
            assert!(err.0.contains(reason), "{err} should mention {reason}");
        }
    }

    #[test]
    fn addresses_may_come_from_server_settings() {
        let text =
            format!("{NOTES}\n[[server_settings]]\nkey = \"office_url\"\nkind = \"text\"\nlabel.en = \"Office\"\n")
                .replace(
                    "files_write = true",
                    "files_write = true\nnetwork = [\"api.example.com\", \"{office_url}\"]\nframe = [\"{office_url}\"]",
                );
        let manifest = Manifest::from_toml(&text).unwrap();
        let setting = |key: &str| (key == "office_url").then(|| "http://192.168.1.20:9980/browser/".to_owned());
        let rules = manifest.permissions.network_rules_with(setting);
        assert!(rules[1].allows("http", "192.168.1.20", 9980));
        assert_eq!(manifest.permissions.network_rules().len(), 1, "without settings: the fixed ones");
        assert_eq!(manifest.permissions.frame_origins(setting), ["http://192.168.1.20:9980"]);
        assert!(manifest.permissions.frame_origins(|_| None).is_empty(), "not filled in yet");
        let https = |_: &str| Some("https://office.example.com".to_owned());
        assert_eq!(manifest.permissions.frame_origins(https), ["https://office.example.com"]);

        let unknown = text.replace("frame = [\"{office_url}\"]", "frame = [\"{nope}\"]");
        assert!(Manifest::from_toml(&unknown).unwrap_err().0.contains("must name a text server setting"));
        let path = text.replace("frame = [\"{office_url}\"]", "frame = [\"https://x.example/path\"]");
        assert!(Manifest::from_toml(&path).unwrap_err().0.contains("permissions.frame"));
    }

    #[test]
    fn a_service_takes_paths_of_the_server_through_a_proxy() {
        let office =
            format!("{NOTES}\n[[server_settings]]\nkey = \"office_url\"\nkind = \"text\"\nlabel.en = \"Office\"\n");
        let with = |proxy: &str| format!("{office}\n[[proxy]]\n{proxy}\n");
        let manifest =
            Manifest::from_toml(&with("target = \"{office_url}\"\npaths = [\"/browser\", \"/cool\", \"/hosting\"]"))
                .unwrap();
        let proxy = &manifest.proxy[0];
        let setting = |key: &str| (key == "office_url").then(|| "http://collabora:9980/".to_owned());
        assert_eq!(proxy.target_origin(setting).as_deref(), Some("http://collabora:9980"));
        assert_eq!(proxy.target_origin(|_| None), None, "not filled in yet");
        let fixed = Manifest::from_toml(&with("target = \"http://127.0.0.1:9980\"\npaths = [\"/cool\"]")).unwrap();
        assert_eq!(fixed.proxy[0].target_origin(|_| None).as_deref(), Some("http://127.0.0.1:9980"));

        assert!(proxy_path_matches("/cool", "/cool"));
        assert!(proxy_path_matches("/cool", "/cool/http%3A%2F%2Fx/ws"));
        assert!(!proxy_path_matches("/cool", "/coolness"));
        assert!(!proxy_path_matches("/cool", "/"));
        for bad in [
            "/api",
            "/app/x",
            "/assets",
            "/health",
            "/",
            "cool",
            "/cool/",
            "/Cool",
            "/a/../b",
            "/a//b",
            "/favicon.svg",
            "/a/b/c/d/e",
        ] {
            let text = with(&format!("target = \"{{office_url}}\"\npaths = [\"{bad}\"]"));
            assert!(Manifest::from_toml(&text).unwrap_err().0.contains("path"), "{bad}");
        }
        let nested = with("target = \"{office_url}\"\npaths = [\"/cool\", \"/cool/ws\"]");
        assert!(Manifest::from_toml(&nested).unwrap_err().0.contains("within each other"));
        let unknown = with("target = \"{nope}\"\npaths = [\"/cool\"]");
        assert!(Manifest::from_toml(&unknown).unwrap_err().0.contains("must name a text server setting"));
        let host_only = with("target = \"collabora:9980\"\npaths = [\"/cool\"]");
        assert!(Manifest::from_toml(&host_only).unwrap_err().0.contains("target"));
        assert!(Manifest::from_toml(&with("target = \"{office_url}\"\npaths = []")).is_err());
    }
}
