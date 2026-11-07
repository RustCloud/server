// Typed client for the RustCloud JSON API.
//
// Every request sends `X-Requested-With`, which the server requires on
// cookie-authenticated requests that change data (CSRF protection).

export interface User {
  id: string;
  username: string;
  display_name: string;
  email: string | null;
  is_admin: boolean;
  disabled: boolean;
  quota_bytes: number | null;
  created_at: string;
}

export interface FileNode {
  id: string;
  parent_id: string | null;
  name: string;
  is_dir: boolean;
  size: number;
  mime_type: string | null;
  etag: string | null;
  created_at: string;
  /** When the server last changed it: content, name or place. */
  updated_at: string;
  /** When its content was changed, as the uploader said (the file's mtime). */
  modified_at: string;
  /** When it went to the trash; only for what is in it. */
  trashed_at?: string;
  /** The lock an app holds while editing it: only that app changes it meanwhile. */
  lock?: { lock_id: string; app_id: string; user_id: string; expires_at: string };
}

export interface Share {
  id: string;
  token: string;
  expires_at: string | null;
  created_at: string;
  node: FileNode;
}

export interface PublicView {
  expires_at: string | null;
  shared: FileNode;
  current: FileNode;
  path: FileNode[];
  children: FileNode[];
}

export interface AuthStatus {
  needs_setup: boolean;
  registration_open: boolean;
  providers: string[];
}

/** A login service on the login page (`GET /api/v1/auth/oidc`). */
export interface OidcButton {
  id: string;
  name: string;
}

/** A way to log in to the caller's account: `password` or `oidc:<service id>`. */
export interface LinkedLogin {
  id: string;
  provider: string;
  created_at: string;
}

/** What the administrator sets for an OpenID Connect login service. */
export interface OidcProviderInput {
  id?: string;
  name: string;
  issuer?: string;
  client_id: string;
  /** Empty keeps the saved secret. */
  client_secret?: string;
  scopes: string;
  enabled: boolean;
  auto_create: boolean;
  link_by_email: boolean;
  groups_claim: string;
  user_groups: string[];
  admin_groups: string[];
  /** The longest a login through it lasts, devices included; 0: no limit. */
  session_days: number;
}

/** A session of the account: a browser or a client app (device). */
export interface SessionInfo {
  id: string;
  device: boolean;
  /** The device's name, or the browser's User-Agent. */
  name: string | null;
  /** `password` or `oidc:<id>`. */
  method: string | null;
  ip: string | null;
  created_at: string;
  last_used_at: string | null;
  expires_at: string;
  /** This browser's session. */
  current: boolean;
}

/** What a client app asks for when it logs in a device (OAuth 2.0 with PKCE). */
export interface AuthorizeRequest {
  client_id: string;
  device_name?: string;
  redirect_uri: string;
  code_challenge: string;
  code_challenge_method: string;
}

/** A login service as the administrator sees it; the secret never comes back. */
export interface OidcProvider extends Required<Omit<OidcProviderInput, 'client_secret'>> {
  has_secret: boolean;
  created_at: string;
  /** The address to register at the service. */
  redirect_url: string;
}

/** An app of the server, as described by its manifest (`GET /api/v1/apps`). */
export interface AppManifest {
  id: string;
  version: string;
  api: string;
  /** Name by language code; `en` is always there. */
  name: Record<string, string>;
  provides?: Record<string, string>;
  /** The most the app may ask users for. */
  permissions?: {
    files?: 'none' | 'opened' | 'folder' | 'all';
    files_write?: boolean;
    network?: string[];
    /** Services the panel may show for its page, at their own addresses. */
    frame?: string[];
    /** Native tools of the server it uses: `ffmpeg`, `onnx`. */
    use?: string[];
  };
  /** The app's entry in the side panel; missing for apps without pages. */
  ui?: { icon: string; entry: string; routes?: string[] };
  /** Discovery addresses: `caldav` → `/dav/`. */
  well_known?: Record<string, string>;
  /** Settings for the whole server that the administrator fills in. */
  server_settings?: ServerSetting[];
  /** Kinds of background jobs with their batch sizes and multiples of the plugin's memory and time. */
  jobs?: { kind: string; batch: number; memory?: number; time?: number }[];
  /** What it needs of the server, as multiples of the base (64 MiB, 10 s); missing: ×1. */
  limits?: Scale;
  /** Files the server downloads into the plugin's storage. */
  downloads?: { path: string; url: string; sha256: string; size?: number }[];
  /** Kinds of files the app opens from the files list. */
  open_with?: OpenWith[];
  /** Paths of this server that go to a service next to it (an office editor). */
  proxy?: { target: string; paths: string[] }[];
  /** Kinds of new files it adds to the files panel's "New" menu. */
  new_files?: NewFile[];
  /** Its path that tells the administrator how it is doing (a JSON list of {@link StatusItem}). */
  status?: string;
}

/** A kind of new file an app offers (`[[new_files]]` in its manifest). */
export interface NewFile {
  /** Without the dot: `docx`. */
  extension: string;
  /** What the menu says and the file's first name, by language. */
  label: Record<string, string>;
  /** The app's path that gives the new file's content; none: empty. */
  template?: string;
}

/** A line of what an app tells about itself on its status path. */
export interface StatusItem {
  level: 'ok' | 'info' | 'warning' | 'error';
  /** Text, or text by language. */
  text: string | Record<string, string>;
}

/** The server's machine and process (`GET /api/v1/admin/system`). */
export interface SystemInfo {
  version: string;
  revision: string | null;
  uptime_seconds: number;
  os: string | null;
  kernel: string | null;
  arch: string;
  container: boolean;
  cpus: number;
  load: [number, number, number] | null;
  memory: { total_bytes: number; available_bytes: number; swap_total_bytes: number; swap_free_bytes: number } | null;
  container_memory: { limit_bytes: number; used_bytes: number } | null;
  process: { memory_bytes: number | null; threads: number | null };
  database: { kind: 'sqlite' | 'postgres'; version: string | null; size_bytes: number | null };
}

/** A kind of files an app opens (`[[open_with]]` in its manifest). */
export interface OpenWith {
  /** `text/markdown`, `image/*`. */
  mime: string[];
  /** The app's page; the panel adds `?file=<id>`. */
  page: string;
  write?: boolean;
  /** The whole browser window (the default) or the panel's page area. */
  layout?: 'full' | 'panel';
  /** What the menu says, by language. */
  label?: Record<string, string>;
}

/** A plugin's setting for the whole server, as its manifest declares it. */
export interface ServerSetting {
  key: string;
  kind: 'bool' | 'number' | 'text' | 'list' | 'secret';
  /** By language code; `en` is always there. */
  label: Record<string, string>;
  hint?: Record<string, string>;
  unit?: string;
}

/** A plugin as the administration lists it (`/api/v1/admin/plugins`). */
export interface Plugin {
  id: string;
  /** `built_in`: part of the server; `shipped`: comes with it; `installed`: added by an administrator. */
  source: 'built_in' | 'shipped' | 'installed';
  /** Missing when the manifest couldn't be read. */
  manifest: AppManifest | null;
  status: { state: 'running' } | { state: 'disabled' } | { state: 'failed'; error: string };
  enabled: boolean;
  /** The multiples the administrator chose; null: as the manifest says. */
  limits: Scale | null;
  /** What it gets, and what follows from that for one request. */
  effective: {
    scale: Scale;
    memory_mib: number;
    /** Most data one call moves at once: a file read whole, a request body. */
    data_mib: number;
    time_seconds: number;
    wall_seconds: number;
    /** The most its background jobs take at once; 0 without jobs. */
    jobs_mib: number;
  };
  /** Values of its server settings, secrets left out. */
  settings: Record<string, unknown>;
  /** Keys of the secrets that are filled in (their values are never sent back). */
  secrets_set: string[];
  /** Its background jobs left (running ones included) and failed, by kind. */
  queue: Record<string, { waiting: number; failed: number }>;
  /** What it keeps in its own storage on the server. */
  storage: { files: number; bytes: number };
  /** Files of its manifest's `[[downloads]]` being fetched, or why that failed; missing when all are there. */
  download?:
    | { state: 'running'; path: string; done: number; total: number | null; left: number }
    | { state: 'failed'; path: string; error: string };
}

/**
 * What a plugin may use, as multiples of a base: memory (×1 = 64 MiB, a
 * quarter of it for data in one call), time (×1 = 10 s of its own work) and
 * batches of background jobs at a time.
 */
export interface Scale {
  memory: number;
  time: number;
  workers: number;
}

/** `use.ffmpeg` on this server: whether it runs, and what confines it. */
export interface FfmpegStatus {
  available: boolean;
  ffmpeg: string | null;
  layers: { rlimits: boolean; no_new_privs: boolean; seccomp: boolean; landlock: boolean } | null;
  /** Why it isn't available, or what is weaker than it should be. */
  problem: string | null;
}

/** `use.onnx` on this server: whether it runs, what confines it, which models are loaded. */
export interface OnnxStatus {
  available: boolean;
  engine: string | null;
  layers: { rlimits: boolean; no_new_privs: boolean; seccomp: boolean; landlock: boolean } | null;
  /** Why it isn't available, or what is weaker than it should be. */
  problem: string | null;
  keep_loaded_seconds: number;
  /** Models in memory now, the most recently used first. */
  loaded: { app_id: string; path: string; idle_seconds: number; busy: boolean }[];
}

/** A file in a plugin's storage. */
export interface PluginStorageFile {
  path: string;
  size: number;
  updated_at: string;
}

/** A version of a plugin in the catalog. */
export interface CatalogRelease {
  version: string;
  /** The plugin API it needs, e.g. `^0.2.16`. */
  api: string;
  url: string;
  sha256: string;
  size: number;
  /** What changed, by language code. */
  notes?: Record<string, string>;
}

/** A plugin of the catalog with what can be done with it on this server (`/api/v1/admin/catalog`). */
export interface CatalogItem {
  id: string;
  name: Record<string, string>;
  description: Record<string, string>;
  author: string;
  license: string;
  source: string;
  homepage?: string;
  versions: CatalogRelease[];
  /** The newest version that works here: what "Install" or "Update" gets. */
  release: CatalogRelease | null;
  /** The installed version, if any. */
  installed: string | null;
  action: 'install' | 'update' | 'latest' | 'needs_newer_server' | 'built_in';
}

export interface CatalogView {
  url: string;
  plugins: CatalogItem[];
  /** Entries of the catalog that were skipped, and why. */
  skipped: string[];
}

export interface PluginUpdate {
  enabled: boolean;
  /** null: as the manifest says. */
  limits: Scale | null;
}

export interface AppsList {
  api_version: string;
  apps: AppManifest[];
  /** By app id: the most a request of its page may carry. */
  limits: Record<string, { data_bytes: number }>;
  /** By app id: origins of the services the panel may show for its page. */
  frames?: Record<string, string[]>;
  /** By app id: paths of this server that go to its service (`[[proxy]]`). */
  proxies?: Record<string, string[]>;
}

/** Access the user gave an app to a file or folder (`/api/v1/grants`). */
export interface Grant {
  id: string;
  app_id: string;
  node: FileNode;
  write: boolean;
  /** `session`: while the file is open in the app; `standing`: until revoked. */
  kind: 'session' | 'standing';
  created_at: string;
  last_seen_at: string;
}

export type AppPasswordAccess = 'read' | 'write';

/** A password for CalDAV and other programs (`/api/v1/app-passwords`). */
export interface AppPassword {
  id: string;
  name: string;
  scopes: { app: string; access: AppPasswordAccess }[];
  created_at: string;
  last_used_at: string | null;
}

/** A user as others see them in shares. */
export interface Person {
  id: string;
  username: string;
  display_name: string;
}

/** A folder of an app's data shared with another user (`/api/v1/app-shares`), e.g. a calendar. */
export interface AppShare {
  id: string;
  app_id: string;
  /** The folder in the owner's data of the app. */
  path: string;
  /** What the app called it when it was shared. */
  label: string;
  owner: Person;
  user: Person;
  write: boolean;
  created_at: string;
}

export interface Usage {
  /** Files, the trash and versions together. */
  used_bytes: number;
  versions_bytes: number;
  quota_bytes: number | null;
}

/** An earlier content of a file. */
export interface Version {
  id: string;
  size: number;
  etag: string;
  modified_by: string | null;
  /** When this content was written. */
  modified_at: string;
  /** When it was replaced. */
  created_at: string;
}

/** Versions of a file, newest first, and how long they are kept (`[files]` settings). */
export interface Versions {
  versions: Version[];
  keep: number;
  days: number;
}

export interface AccountUpdate {
  display_name?: string;
  is_admin?: boolean;
  disabled?: boolean;
  quota_bytes?: number | null;
  password?: string;
}

export interface SettingsField {
  key: string;
  kind: 'bool' | 'number' | 'text' | 'list';
  /** For numbers: unit such as `days` or `MiB`, and whether empty ("no limit") is allowed. */
  unit?: string | null;
  optional?: boolean;
}

/** One settings section of the server or an app (`[server]`, `[accounts]`, …). */
export interface SettingsSection {
  name: string;
  fields: SettingsField[];
  values: Record<string, unknown>;
  /** Keys set by environment variables; they can't be changed here. */
  locked: string[];
}

export interface SpeedTestResult {
  bytes: number;
  /** MiB per second. */
  write_speed: number;
  read_speed: number;
  tested_at: string;
}

export interface StorageReport {
  kind: string;
  location: string;
  capacity: { total_bytes: number; free_bytes: number } | null;
  used_percent: number | null;
  nearly_full: boolean;
  stored_bytes: number;
  file_count: number;
  speed_test: SpeedTestResult | null;
}

export interface JournalEntry {
  id: number;
  time: string;
  level: 'ERROR' | 'WARN' | 'INFO';
  /** What happened, e.g. `file.download`; null for plain warnings and errors. */
  action: string | null;
  message: string;
  username: string | null;
  ip: string | null;
  details: Record<string, unknown>;
}

export interface JournalPage {
  entries: JournalEntry[];
  /** Events lost because the server could not write them in time. */
  dropped: number;
}

/** An error answered by the server: `{"error": {"code", "message"}}`. */
export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
  ) {
    super(message);
  }
}

const CSRF_HEADER = { 'X-Requested-With': 'rustcloud' };

/** Called when the server says the session is gone (401). */
let onUnauthorized: () => void = () => {};
export function setUnauthorizedHandler(handler: () => void) {
  onUnauthorized = handler;
}

async function errorFrom(response: Response): Promise<ApiError> {
  try {
    const body = await response.json();
    return new ApiError(response.status, body.error.code, body.error.message);
  } catch {
    return new ApiError(response.status, 'HTTP_' + response.status, response.statusText);
  }
}

/** A request to the API; a `Blob` body (a file) is sent as it is, anything else as JSON. */
async function request<T>(method: string, path: string, body?: unknown): Promise<T> {
  const headers: Record<string, string> = { ...CSRF_HEADER };
  const raw = body instanceof Blob;
  if (body !== undefined && !raw) headers['Content-Type'] = 'application/json';

  const response = await fetch(path, {
    method,
    headers,
    body: body === undefined ? undefined : raw ? body : JSON.stringify(body),
    credentials: 'same-origin',
  });
  if (!response.ok) {
    const error = await errorFrom(response);
    if (response.status === 401 && !path.startsWith('/api/v1/auth/')) onUnauthorized();
    throw error;
  }
  return response.status === 204 ? (undefined as T) : response.json();
}

const enc = encodeURIComponent;

/** GET of an app's own path (`/app/<id><path>`); not 2xx is an error. */
async function appGet(appId: string, path: string): Promise<Response> {
  const response = await fetch(`/app/${enc(appId)}${path}`, { credentials: 'same-origin' });
  if (!response.ok) throw new ApiError(response.status, 'HTTP_' + response.status, response.statusText);
  return response;
}

/** The status items of an app's answer; anything else is an error. */
export function statusItems(answer: unknown): StatusItem[] {
  const levels = ['ok', 'info', 'warning', 'error'];
  const isText = (text: unknown) =>
    typeof text === 'string' ||
    (!!text && typeof text === 'object' && Object.values(text).every((value) => typeof value === 'string'));
  const valid =
    Array.isArray(answer) && answer.every((item) => item && levels.includes(item.level) && isText(item.text));
  if (!valid) throw new ApiError(0, 'BAD_STATUS', 'The plugin answered something other than a list of status lines');
  return answer;
}

export const api = {
  apps: () => request<AppsList>('GET', '/api/v1/apps'),
  grants: () => request<Grant[]>('GET', '/api/v1/grants'),
  createGrant: (grant: { app_id: string; node_id: string; write: boolean; kind: 'session' | 'standing' }) =>
    request<Grant>('POST', '/api/v1/grants', grant),
  revokeGrant: (id: string) => request<void>('DELETE', `/api/v1/grants/${id}`),
  /** The page with the file was closed: the access lasts a minute more for a service the app works with. */
  closeGrant: (id: string) => request<void>('DELETE', `/api/v1/grants/${id}?closing=true`),
  keepGrant: (id: string) => request<void>('POST', `/api/v1/grants/${id}/keep-alive`),
  appPasswords: () => request<AppPassword[]>('GET', '/api/v1/app-passwords'),
  /** The response is the only time the password is shown. */
  createAppPassword: (name: string, scopes: AppPassword['scopes']) =>
    request<AppPassword & { password: string }>('POST', '/api/v1/app-passwords', { name, scopes }),
  revokeAppPassword: (id: string) => request<void>('DELETE', `/api/v1/app-passwords/${id}`),
  appShares: () => request<{ given: AppShare[]; received: AppShare[] }>('GET', '/api/v1/app-shares'),
  shareAppData: (share: { app_id: string; path: string; label: string; username: string; write: boolean }) =>
    request<AppShare>('POST', '/api/v1/app-shares', share),
  revokeAppShare: (id: string) => request<void>('DELETE', `/api/v1/app-shares/${id}`),

  // Account
  status: () => request<AuthStatus>('GET', '/api/v1/auth/status'),
  login: (username: string, password: string) =>
    request<{ user: User }>('POST', '/api/v1/auth/login', { username, password }),
  register: (username: string, password: string, display_name?: string) =>
    request<{ user: User }>('POST', '/api/v1/auth/register', { username, password, display_name }),
  logout: () => request<void>('POST', '/api/v1/auth/logout'),
  me: () => request<User>('GET', '/api/v1/auth/me'),
  updateProfile: (update: { display_name?: string; email?: string }) =>
    request<User>('PATCH', '/api/v1/auth/me', update),
  changePassword: (current_password: string, new_password: string, revoke_app_passwords = false) =>
    request<void>('POST', '/api/v1/auth/password', { current_password, new_password, revoke_app_passwords }),
  /** Ends all sessions of the user except this one. */
  logoutOthers: () => request<{ ended: number }>('POST', '/api/v1/auth/logout-others'),
  oidcButtons: () => request<OidcButton[]>('GET', '/api/v1/auth/oidc'),
  /** The browser goes here to log in through a service. */
  oidcLoginUrl: (id: string, returnTo?: string) =>
    `/api/v1/auth/oidc/${enc(id)}/login${returnTo ? `?return=${enc(returnTo)}` : ''}`,
  /** Starts adding a service to the account; the browser then goes to `url`. */
  oidcLink: (id: string) => request<{ url: string }>('POST', `/api/v1/auth/oidc/${enc(id)}/link`),
  linkedLogins: () => request<LinkedLogin[]>('GET', '/api/v1/auth/logins'),
  unlinkLogin: (id: string) => request<void>('DELETE', `/api/v1/auth/logins/${id}`),
  sessions: () => request<SessionInfo[]>('GET', '/api/v1/auth/sessions'),
  endSession: (id: string) => request<void>('DELETE', `/api/v1/auth/sessions/${id}`),
  /** Allows a client app; it gets the code through its redirect URI. */
  authorize: (asked: AuthorizeRequest) => request<{ code: string }>('POST', '/api/v1/auth/authorize', asked),

  // Files
  root: () => request<FileNode>('GET', '/api/v1/files/root'),
  details: (id: string) => request<{ node: FileNode; path: FileNode[] }>('GET', `/api/v1/files/${id}`),
  children: (id: string) => request<FileNode[]>('GET', `/api/v1/files/${id}/children`),
  createFolder: (parentId: string, name: string) =>
    request<FileNode>('POST', `/api/v1/files/${parentId}/folders`, { name }),
  update: (id: string, update: { name?: string; parent_id?: string }) =>
    request<FileNode>('PATCH', `/api/v1/files/${id}`, update),
  copy: (id: string, parent_id: string, name?: string) =>
    request<FileNode>('POST', `/api/v1/files/${id}/copy`, { parent_id, name }),
  remove: (id: string) => request<void>('DELETE', `/api/v1/files/${id}`),
  trash: () => request<FileNode>('GET', '/api/v1/files/trash'),
  restore: (id: string) => request<FileNode>('POST', `/api/v1/files/${id}/restore`),
  emptyTrash: () => request<void>('DELETE', '/api/v1/files/trash'),
  search: (query: string) => request<FileNode[]>('GET', `/api/v1/files/search?q=${enc(query)}`),
  usage: () => request<Usage>('GET', '/api/v1/usage'),
  zipUrl: (id: string) => `/api/v1/files/${id}/zip`,
  contentUrl: (id: string, download = false) => `/api/v1/files/${id}/content${download ? '?download=1' : ''}`,
  unlock: (id: string) => request<void>('DELETE', `/api/v1/files/${id}/lock`),
  versions: (id: string) => request<Versions>('GET', `/api/v1/files/${id}/versions`),
  restoreVersion: (id: string, version: string) =>
    request<FileNode>('POST', `/api/v1/files/${id}/versions/${version}/restore`),
  /** A new file with `content`; NAME_TAKEN if the folder has one of that name. */
  createFile: (parentId: string, name: string, content: Blob) =>
    request<FileNode>('POST', `/api/v1/files/${parentId}/upload?name=${enc(name)}`, content),
  versionUrl: (id: string, version: string) => `/api/v1/files/${id}/versions/${version}/content?download=1`,

  // Public links
  shares: () => request<Share[]>('GET', '/api/v1/shares'),
  createShare: (node_id: string, expires_at: string | null) =>
    request<Share>('POST', '/api/v1/shares', { node_id, expires_at }),
  deleteShare: (id: string) => request<void>('DELETE', `/api/v1/shares/${id}`),
  publicView: (token: string, nodeId?: string) =>
    request<PublicView>('GET', `/api/v1/public/${enc(token)}${nodeId ? `?node=${nodeId}` : ''}`),
  publicZipUrl: (token: string, nodeId?: string) =>
    `/api/v1/public/${enc(token)}/zip${nodeId ? `?node=${nodeId}` : ''}`,
  publicContentUrl: (token: string, nodeId?: string, download = false) => {
    const query = new URLSearchParams();
    if (nodeId) query.set('node', nodeId);
    if (download) query.set('download', '1');
    const qs = query.toString();
    return `/api/v1/public/${enc(token)}/content${qs ? '?' + qs : ''}`;
  },

  // Server
  info: () => request<{ version: string; public_url: string }>('GET', '/api/v1/info'),

  // Apps' own paths (with the user's session, like their pages)
  /** The content of a new file from an app's template path. */
  appTemplate: async (appId: string, path: string) => (await appGet(appId, path)).blob(),
  /** What an app tells about itself on its status path; a broken answer is an error. */
  appStatus: async (appId: string, path: string) => statusItems(await (await appGet(appId, path)).json()),

  // Administration
  system: () => request<SystemInfo>('GET', '/api/v1/admin/system'),
  settings: () => request<SettingsSection[]>('GET', '/api/v1/admin/settings'),
  oidcProviders: () => request<OidcProvider[]>('GET', '/api/v1/admin/oidc'),
  addOidcProvider: (input: OidcProviderInput) => request<OidcProvider>('POST', '/api/v1/admin/oidc', input),
  changeOidcProvider: (id: string, input: OidcProviderInput) =>
    request<OidcProvider>('PUT', `/api/v1/admin/oidc/${enc(id)}`, input),
  removeOidcProvider: (id: string) => request<void>('DELETE', `/api/v1/admin/oidc/${enc(id)}`),
  plugins: () => request<Plugin[]>('GET', '/api/v1/admin/plugins'),
  ffmpegStatus: () => request<FfmpegStatus>('GET', '/api/v1/admin/use/ffmpeg'),
  onnxStatus: () => request<OnnxStatus>('GET', '/api/v1/admin/use/onnx'),
  configurePlugin: (id: string, update: PluginUpdate) =>
    request<Plugin>('PUT', `/api/v1/admin/plugins/${encodeURIComponent(id)}`, update),
  /** Installs or updates a plugin from a zip archive. */
  installPlugin: (archive: File) => request<Plugin>('POST', '/api/v1/admin/plugins', archive),
  /** The plugin catalog; `fresh` downloads it again instead of using the copy the server keeps. */
  pluginCatalog: (fresh = false) => request<CatalogView>('GET', `/api/v1/admin/catalog${fresh ? '?fresh=true' : ''}`),
  installFromCatalog: (id: string, version: string) =>
    request<Plugin>('POST', `/api/v1/admin/catalog/${encodeURIComponent(id)}/install`, { version }),
  /** A secret missing or empty keeps its value, `null` clears it. */
  savePluginSettings: (id: string, values: Record<string, unknown>) =>
    request<Plugin>('PUT', `/api/v1/admin/plugins/${encodeURIComponent(id)}/settings`, values),
  removePlugin: (id: string) => request<void>('DELETE', `/api/v1/admin/plugins/${encodeURIComponent(id)}`),
  pluginStorage: (id: string) =>
    request<PluginStorageFile[]>('GET', `/api/v1/admin/plugins/${encodeURIComponent(id)}/storage`),
  /** Downloads of the manifest start again afterwards. */
  clearPluginStorage: (id: string) =>
    request<Plugin>('DELETE', `/api/v1/admin/plugins/${encodeURIComponent(id)}/storage`),
  retryPluginDownloads: (id: string) =>
    request<Plugin>('POST', `/api/v1/admin/plugins/${encodeURIComponent(id)}/downloads`),
  saveSettings: (section: string, values: Record<string, unknown>) =>
    request<SettingsSection>('PUT', `/api/v1/admin/settings/${enc(section)}`, values),
  journal: (filter: { problems?: boolean; q?: string; before?: number }) => {
    const query = new URLSearchParams();
    if (filter.problems) query.set('problems', 'true');
    if (filter.q) query.set('q', filter.q);
    if (filter.before) query.set('before', String(filter.before));
    return request<JournalPage>('GET', `/api/v1/admin/journal?${query}`);
  },
  storage: () => request<StorageReport>('GET', '/api/v1/admin/storage'),
  speedTest: () => request<SpeedTestResult>('POST', '/api/v1/admin/storage/speed-test'),
  users: () => request<User[]>('GET', '/api/v1/admin/users'),
  createUser: (account: { username: string; password: string; display_name?: string; is_admin: boolean }) =>
    request<User>('POST', '/api/v1/admin/users', account),
  updateUser: (id: string, update: AccountUpdate) => request<User>('PATCH', `/api/v1/admin/users/${id}`, update),
  deleteUser: (id: string) => request<void>('DELETE', `/api/v1/admin/users/${id}`),
};

/** A running upload that reports progress and can be cancelled. */
export interface Upload {
  done: Promise<FileNode>;
  cancel: () => void;
}

/**
 * Uploads one file into a folder. Uses XMLHttpRequest because fetch cannot
 * report upload progress. The body is the raw file — no multipart encoding.
 */
export function uploadFile(
  folderId: string,
  file: File,
  overwrite: boolean,
  onProgress: (fraction: number) => void,
): Upload {
  const xhr = new XMLHttpRequest();
  const done = new Promise<FileNode>((resolve, reject) => {
    let query = `name=${enc(file.name)}${overwrite ? '&overwrite=true' : ''}`;
    // The file keeps its time — unless the computer's clock says something odd.
    if (file.lastModified > 0 && file.lastModified < Date.now() + 12 * 3600_000) {
      query += `&mtime=${file.lastModified / 1000}`;
    }
    xhr.open('POST', `/api/v1/files/${folderId}/upload?${query}`);
    xhr.setRequestHeader('X-Requested-With', 'rustcloud');
    xhr.upload.onprogress = (event) => {
      if (event.lengthComputable) onProgress(event.loaded / event.total);
    };
    xhr.onload = () => {
      if (xhr.status >= 200 && xhr.status < 300) {
        resolve(JSON.parse(xhr.responseText));
        return;
      }
      try {
        const body = JSON.parse(xhr.responseText);
        reject(new ApiError(xhr.status, body.error.code, body.error.message));
      } catch {
        reject(new ApiError(xhr.status, 'HTTP_' + xhr.status, xhr.statusText));
      }
    };
    xhr.onerror = () => reject(new ApiError(0, 'NETWORK', 'Network error'));
    xhr.onabort = () => reject(new ApiError(0, 'CANCELLED', 'Cancelled'));
    xhr.send(file);
  });
  return { done, cancel: () => xhr.abort() };
}
