// UI texts. The language follows the browser (Russian or English).
//
// `ru` must have exactly the same keys as `en` — TypeScript checks that, so a
// missing translation is a build error, not a blank button.

/** Mebibytes in words: `256 MiB`, `1 GiB`. */
const sizeIn = (mib: string, gib: string) => (n: number) => (n >= 1024 ? `${n / 1024} ${gib}` : `${n} ${mib}`);
const sizeEn = sizeIn('MiB', 'GiB');
const sizeRu = sizeIn('МиБ', 'ГиБ');

/** Russian noun for a count: `ruCount(2, 'версия', 'версии', 'версий')` → `2 версии`. */
function ruCount(n: number, one: string, few: string, many: string): string {
  const tens = n % 100;
  const last = n % 10;
  const word = tens >= 11 && tens <= 14 ? many : last === 1 ? one : last >= 2 && last <= 4 ? few : many;
  return `${n} ${word}`;
}

const en = {
  appName: 'RustCloud',
  loading: 'Loading…',
  save: 'Save',
  cancel: 'Cancel',
  close: 'Close',
  create: 'Create',
  delete: 'Delete',
  copy: 'Copy',
  saved: 'Saved',
  never: 'Never',
  unlimited: 'Unlimited',

  // Login and setup
  login: 'Log in',
  logout: 'Log out',
  username: 'Username',
  password: 'Password',
  displayName: 'Display name',
  email: 'Email',
  createAccount: 'Create account',
  noAccount: 'No account yet?',
  haveAccount: 'Already have an account?',
  setupTitle: 'Welcome to RustCloud',
  setupHint: 'Create the first account. It becomes the administrator.',

  // Navigation
  apps: 'Apps',
  files: 'Files',
  myFiles: 'My files',
  links: 'Public links',
  settings: 'Settings',
  administration: 'Administration',
  collapse: 'Collapse',
  expand: 'Expand',
  searchPlaceholder: 'Search files…',

  // Files
  home: 'Home',
  upload: 'Upload',
  uploadFolder: 'Upload folder',
  newFolder: 'New folder',
  newItem: 'New',
  newFileTitle: (kind: string) => `New: ${kind}`,
  textFile: 'Text file',
  textFileName: 'New text file',
  markdownFile: 'Markdown text',
  markdownFileName: 'Notes',
  folderName: 'Folder name',
  name: 'Name',
  size: 'Size',
  modified: 'Modified',
  emptyFolder: 'This folder is empty. Drop files or folders here or use “Upload”.',
  dropHere: 'Drop files or folders to upload',
  dropInto: (n: number, folder: string) => (n === 1 ? `1 item → “${folder}”` : `${n} items → “${folder}”`),
  uploadWaiting: 'Waiting',
  download: 'Download',
  rename: 'Rename',
  move: 'Move',
  share: 'Share',
  moveTo: 'Move to',
  copyTo: 'Copy to',
  chooseFolder: 'Choose this folder',
  up: 'Up',
  selected: (n: number) => `${n} selected`,
  deleteSelected: 'Delete selected',
  confirmDelete: (name: string) => `Delete “${name}” forever? This cannot be undone.`,
  confirmDeleteMany: (n: number) => `Delete ${n} items forever? This cannot be undone.`,
  trash: 'Trash',
  restore: 'Restore',
  deleteForever: 'Delete forever',
  versions: 'Versions',
  lockedBy: (app: string) => `Being edited in “${app}”: only it changes the file meanwhile`,
  unlock: 'Unlock',
  confirmUnlock: (app: string) =>
    `Unlock the file? “${app}” may lose changes it hasn’t saved yet. Do this only if the editor is gone.`,
  currentVersion: 'Current',
  noVersions: 'No earlier versions yet: when the file is replaced, its old content is kept here.',
  versionsKept: (keep: number, days: number) =>
    keep && days
      ? `The latest ${keep} versions are kept, and all from the last ${days} days.`
      : keep
        ? `The latest ${keep} versions are kept.`
        : days
          ? `Versions from the last ${days} days are kept.`
          : 'The administrator has turned versions off.',
  versionsQuota: 'Versions count against your storage; when space runs out, the oldest go first.',
  emptyTrash: 'Empty trash',
  confirmEmptyTrash: 'Delete everything in the trash forever? This cannot be undone.',
  trashEmpty: 'The trash is empty.',
  trashHint:
    'Deleted files and folders stay here until you delete them forever. The administrator may set how long they are kept.',
  confirmOverwrite: (name: string) => `“${name}” already exists. Replace it?`,
  uploads: 'Uploads',
  uploadDone: 'Done',
  uploadFailed: 'Failed',
  searchResults: (query: string) => `Search: “${query}”`,
  nothingFound: 'Nothing found.',
  openFolder: 'Open folder',
  previous: 'Previous',
  next: 'Next',
  noPreview: 'No preview for this file type.',
  previewFailed: 'The preview could not be loaded.',
  previewTruncated: (size: string) => `Only the first ${size} are shown. Download the file to see all of it.`,

  // Links
  createLink: 'Create link',
  linkExpires: 'Expires',
  linkExpiresHint: 'Leave empty for a link that never expires.',
  copyLink: 'Copy link',
  linkCopied: 'Link copied',
  deleteLink: 'Delete link',
  noLinks: 'You have not shared anything yet.',
  sharedItem: 'Shared item',
  existingLinks: 'Existing links',

  // Public page
  sharedWithYou: 'Shared with you',
  linkGone: 'This link does not exist or has expired.',
  downloadFile: 'Download file',

  // Settings
  profile: 'Profile',
  changePassword: 'Change password',
  currentPassword: 'Current password',
  newPassword: 'New password',
  passwordChanged: 'Password changed. Other sessions were logged out.',
  storage: 'Storage',
  usedOf: (used: string, total: string) => `${used} of ${total} used`,
  usedNoLimit: (used: string) => `${used} used, no limit`,
  versionsUse: (size: string) => `Of that, earlier versions of files: ${size}.`,
  display: 'Display',
  showHidden: 'Show hidden files',
  showHiddenHint: 'Files and folders whose name starts with a dot, such as app data (.appdata). Shown faded.',
  appAccess: 'App access',
  appAccessNone: 'No app has access to your files.',
  accessRead: 'read',
  accessWrite: 'read and change',
  accessWhileOpen: 'while open',
  openIn: (app: string) => `Open in “${app}”`,
  openFileTitle: 'Open the file in the app?',
  openFileText: (app: string, file: string, write: boolean) =>
    `“${app}” will ${write ? 'read and change' : 'read'} “${file}” while it is open, and nothing else.`,
  rememberForApp: 'Don’t ask again for this app in this browser',
  open: 'Open',
  appCannotOpen: 'This app does not open such files.',
  accessStanding: 'until revoked',
  accessSince: (date: string) => `since ${date}`,
  revoke: 'Revoke',
  accessRequestTitle: (app: string) => `“${app}” asks for a folder`,
  accessRequestText: (app: string) =>
    `“${app}” asks for access to one folder of your files until you revoke it. It won’t see your other files.`,
  accessCreateFolder: (name: string) => `Create the folder “${name}”`,
  accessPickFolder: 'Choose an existing folder…',
  accessPickedFolder: (name: string) => `Use the folder “${name}”`,
  accessPickAnother: 'Choose another…',
  accessPickTitle: 'Folder for the app',
  accessWillWrite: 'The app will be able to read, change and delete files in this folder.',
  accessWillRead: 'The app will be able to read files in this folder, not change them.',
  accessAllow: 'Allow',
  appPasswords: 'App passwords',
  appPasswordsHint:
    'For calendar apps and other programs that ask for a username and password. Each program gets its own password with only the access it needs; your account password stays private.',
  appPasswordNew: 'New app password',
  appPasswordName: 'Name',
  appPasswordNamePlaceholder: 'Phone calendar',
  appPasswordNoAccess: 'no access',
  appPasswordChooseApp: 'Choose at least one app.',
  appPasswordCreated: 'Copy the password now: it won’t be shown again. Log in with your username and this password.',
  appPasswordCopied: 'Copied',
  appPasswordUsed: (date: string) => `last used ${date}`,
  appPasswordNeverUsed: 'not used yet',
  appShares: 'Shared app data',
  appSharesNone: 'Nothing shared yet. Apps such as Calendar offer to share from their pages.',
  appSharesGiven: 'You shared',
  appSharesReceived: 'Shared with you',
  appShareWith: (label: string, who: string) => `“${label}” with ${who}`,
  appShareFrom: (label: string, who: string) => `“${label}” from ${who}`,
  appShareLeave: 'Remove',
  appShareTitle: (label: string) => `Share “${label}”`,
  appShareText: (app: string) =>
    `People you add see this in their “${app}” and in the programs they connect to it. You can stop sharing at any time.`,
  appShareUsername: 'Username',
  appShareAdd: 'Share',
  appShareNobody: 'Not shared with anyone yet.',
  appShareCanRead: 'can view',
  appShareCanWrite: 'can edit',
  pluginsTitle: 'Plugins of this server',
  pluginSource: { built_in: 'part of the server', shipped: 'comes with the server', installed: 'installed' } as Record<
    string,
    string
  >,
  pluginState: { running: 'Running', disabled: 'Disabled', failed: 'Can’t run' } as Record<string, string>,
  pluginEnable: 'Enable',
  pluginDisable: 'Disable',
  pluginResources: (
    e: { memory_mib: number; data_mib: number; time_seconds: number; scale: { workers: number } },
    own: boolean,
  ) =>
    `Gets ${sizeEn(e.memory_mib)} of memory, files up to ${sizeEn(e.data_mib)} at once, ${e.time_seconds} s of work, ` +
    `${e.scale.workers} batch(es) of jobs at a time` +
    (own ? ' (set by the administrator)' : ''),
  pluginAsks: (s: { memory: number; time: number; workers: number }) =>
    `asks for: memory ×${s.memory}, time ×${s.time}, ${s.workers} batch(es) at a time`,
  pluginAdvanced: 'Advanced settings',
  pluginMemory: 'Memory',
  pluginMemoryStep: (step: number) => `${sizeEn(64 * step)}, files up to ${sizeEn(16 * step)}`,
  pluginTime: 'Time',
  pluginTimeStep: (step: number) => `${10 * step} s of work, ${60 * step} s in all`,
  pluginWorkers: 'Batches of jobs at a time',
  pluginAsManifest: 'As the manifest says',
  pluginAdvancedHint: 'Everything else follows from these: files read at once, request bodies, time in all.',
  pluginsJobsMemory: (mib: number) => `Background jobs of the plugins take up to ${sizeEn(mib)} of memory at once.`,
  pluginFiles: {
    none: 'no access to files',
    opened: 'files the user opens in it',
    folder: 'folders the user gives it',
    all: 'all files the user gives it',
  } as Record<string, string>,
  pluginFilesWrite: 'may change them',
  pluginAddresses: 'Addresses',
  pluginNetwork: 'Network',
  pluginUses: 'Uses',
  pluginFrames: 'Shows services',
  pluginProxy: (paths: string[], target: string) =>
    `Shows the service ${target} at ${paths.join(', ')} of this server: it acts for users who open files in the plugin — approve only a service you trust`,
  ffmpegRuns: (version: string, layers: string) => `use.ffmpeg: ffmpeg ${version}; confined by ${layers}.`,
  ffmpegMissing: (why: string) => `use.ffmpeg is not available: ${why}`,
  onnxRuns: (engine: string, minutes: number) =>
    `use.onnx: ${engine}; a model stays loaded ${minutes} min after its last run.`,
  onnxLoaded: (models: string[]) => (models.length ? `In memory: ${models.join(', ')}.` : 'No models in memory.'),
  onnxMissing: (why: string) => `use.onnx is not available: ${why}`,
  pluginEnableNote: 'Enabling a plugin approves what it may ask users for and the servers it may reach.',
  pluginInstall: 'Install plugin…',
  pluginInstallHint:
    'A .zip with manifest.toml and app.wasm. A new plugin starts disabled; an update stays enabled unless it asks for more.',
  pluginInstalled: (name: string, version: string) => `Installed ${name} ${version}.`,
  pluginRemove: 'Remove',
  pluginSettings: 'Settings',
  pluginConfigure: 'Configure',
  pluginTabSettings: 'Settings',
  pluginTabStatus: 'Status',
  pluginTabSystem: 'System',
  pluginNoSettings: 'The plugin has no settings of its own.',
  pluginReport: 'What the plugin says',
  pluginReportFailed: (why: string) => `The plugin did not answer: ${why}`,
  pluginReportEmpty: 'Nothing to report.',
  pluginRights: 'What it may do',
  pluginSecretSet: 'saved; type to replace',
  pluginSecretClear: 'Clear',
  pluginSettingsSaved: 'Saved.',
  pluginConfirmRemove: (name: string) =>
    `Remove the plugin “${name}” with its storage? Users’ data of it stays and comes back if you install it again.`,
  pluginStorage: (size: string, files: number) => `Storage: ${size}, files: ${files}`,
  pluginStorageFiles: 'Files',
  pluginStorageClear: 'Clear',
  pluginConfirmClear: (name: string) =>
    `Clear the storage of “${name}”? The files it downloads come again; everything else is lost.`,
  pluginDownloading: (path: string, progress: string, left: number) =>
    `Downloading ${path}: ${progress}${left > 1 ? `; files left: ${left}` : ''}`,
  pluginDownloadFailed: (path: string, error: string) => `Couldn’t download ${path}: ${error}`,
  pluginRetry: 'Try again',
  pluginJobs: 'Background jobs',
  pluginJobsLeft: (waiting: number, failed: number) => `left: ${waiting}, failed: ${failed}`,
  pluginJobsBatch: (batch: number) => `${batch} per batch`,
  done: 'Done',

  // Admin
  users: 'Users',
  serverTab: 'Server',
  systemTitle: 'Server and system',
  systemVersion: 'Version',
  systemUptime: 'Running for',
  systemOs: 'System',
  systemKernel: 'kernel',
  systemContainer: 'in a container',
  systemCpu: 'Processor',
  systemCpus: (n: number) => `${n} core${n === 1 ? '' : 's'}`,
  systemLoad: (load: string) => `load ${load} (1 / 5 / 15 min)`,
  systemMemory: 'Memory',
  systemSwap: (usage: string) => `Swap: ${usage}`,
  systemContainerMemory: 'Container limit',
  systemProcess: 'RustCloud process',
  systemProcessMemory: (size: string) => `${size} of memory`,
  systemThreads: (n: number) => `${n} threads`,
  systemDatabase: 'Database',
  duration: (seconds: number) => {
    const days = Math.floor(seconds / 86400);
    const hours = Math.floor((seconds % 86400) / 3600);
    const minutes = Math.floor((seconds % 3600) / 60);
    return days ? `${days} d ${hours} h` : hours ? `${hours} h ${minutes} min` : `${minutes} min`;
  },
  newUser: 'New user',
  isAdmin: 'Administrator',
  disabled: 'Disabled',
  quotaMib: 'Quota, MiB',
  created: 'Created',
  resetPassword: 'Reset password',
  newPasswordFor: (name: string) => `New password for ${name}:`,
  you: 'you',

  // Administration → storage
  storageTitle: 'Storage',
  storageKinds: { local: 'Local disk' } as Record<string, string>,
  diskUsage: (used: string, total: string, percent: string) => `${used} of ${total} used (${percent}%)`,
  diskFree: (free: string) => `${free} free`,
  storedFiles: (size: string, count: number) => `RustCloud files: ${size} in ${count} files`,
  nearlyFull: (percent: string) => `The storage disk is ${percent}% full. Free up space or add more.`,
  speedTest: 'Speed',
  speedResult: (write: string, read: string) => `write ${write} MiB/s, read ${read} MiB/s`,
  speedTestedAt: (when: string, size: string) => `measured ${when} with ${size}`,
  speedTestRun: 'Test speed',
  speedTestRunning: 'Testing…',
  speedNever: 'Not measured yet.',

  // Administration → journal
  journalProblems: 'Only warnings and errors',
  journalSearch: 'Search: user, file, address…',
  journalRefresh: 'Refresh',
  journalMore: 'Show older',
  journalEmpty: 'No entries.',
  journalTime: 'Time',
  journalUser: 'User',
  journalAddress: 'Address',
  journalEvent: 'Event',
  journalDropped: (n: number) => `${n} events were lost because the log could not be written in time.`,
  journalActions: {
    'server.started': () => 'Server started',
    'auth.login': () => 'Logged in',
    'auth.login_failed': () => 'Failed login',
    'auth.logout': () => 'Logged out',
    'auth.password_change': () => 'Changed own password',
    'account.create': (d) => `Account “${d.account}” created`,
    'account.update': (d) => `Account “${d.account}” changed`,
    'folder.create': (d) => `Created folder “${d.name}”`,
    'file.upload': (d) => `Uploaded “${d.name}”`,
    'file.version.download': (d) => `Downloaded an earlier version of “${d.name}”`,
    'file.version.restore': (d) => `Restored an earlier version of “${d.name}”`,
    'file.download': (d) => `Downloaded “${d.name}”`,
    'file.rename': (d) => `Renamed “${d.name}” to “${d.new_name}”`,
    'file.move': (d) => `Moved “${d.name}”`,
    'file.copy': (d) => `Copied “${d.name}”`,
    'file.delete': (d) => `Deleted “${d.name}”`,
    'share.create': (d) => `Created a public link to “${d.name}”`,
    'share.delete': () => 'Deleted a public link',
    'share.download': (d) => `“${d.name}” downloaded through a public link`,
    'settings.save': (d) => `Saved settings: ${d.section}`,
    'grant.create': (d) => `Gave app “${d.app}” access to “${d.name}”`,
    'grant.revoke': (d) => `Access of app “${d.app}” ended`,
    'app_password.create': (d) => `Created the app password “${d.name}” (${d.scopes})`,
    'app_password.revoke': (d) => `Revoked the app password “${d.name}”`,
    'app_share.create': (d) =>
      `Shared “${d.name}” of app “${d.app}” with ${d.with} (${String(d.write) === 'true' ? 'can edit' : 'can view'})`,
    'app_share.revoke': (d) =>
      d.by === 'owner'
        ? `Stopped sharing “${d.name}” of app “${d.app}”`
        : `Removed the shared “${d.name}” of app “${d.app}”`,
    'storage.speed_test': (d) => `Storage speed test: write ${d.write} MiB/s, read ${d.read} MiB/s`,
  } as Record<string, (details: Record<string, unknown>) => string>,

  // Administration → settings sections. Unknown sections and fields show their keys.
  settingFromEnv: 'Set by an environment variable; change it there.',
  units: {
    days: 'days',
    MiB: 'MiB',
    attempts: 'attempts',
    s: 's',
    batches: 'batches',
    processes: 'processes',
    MP: 'MP',
    x64MiB: '× 64 MiB',
    versions: 'versions',
    connections: 'connections',
    requests: 'requests',
  } as Record<string, string>,
  sections: {
    server: {
      title: 'Settings',
      fields: {
        public_url: [
          'Public address',
          'The address users open, e.g. https://cloud.example.com. Public links are built from it (when empty, from the current browser address). With https:// cookies are sent over HTTPS only.',
        ],
        other_urls: ['Other addresses', 'One per line, e.g. a LAN address: http://192.168.1.10:8080'],
        only_listed_urls: [
          'Answer only on these addresses',
          'Requests to other host names are refused (localhost always works). Protects against DNS rebinding.',
        ],
        cors_origins: [
          'Sites allowed to use the API (CORS)',
          'One per line, e.g. https://app.example.com. They log in with tokens; cookies are never shared.',
        ],
        trusted_proxies: [
          'Trusted reverse proxies',
          'Addresses or networks, one per line (e.g. 172.16.0.0/12). Their X-Forwarded-For and X-Forwarded-Proto are trusted.',
        ],
      },
    },
    accounts: {
      title: 'Accounts',
      fields: {
        open_registration: ['Anyone can create an account', 'Otherwise only administrators create accounts.'],
        session_days: ['Login lasts', ''],
        default_quota_mib: [
          'Storage quota for new accounts',
          'Empty means unlimited. Existing accounts keep their quota.',
        ],
        max_login_failures: [
          'Failed logins before a 15-minute block',
          'Per account; one IP address may fail five times as often.',
        ],
      },
    },
    files: {
      title: 'Files',
      fields: {
        max_upload_mib: ['Largest upload', 'Empty means unlimited.'],
        trash_days: ['Days in the trash', 'After that, deleted files are gone for good; 0 keeps them until emptied.'],
        versions_keep: ['Versions kept in any case', 'The latest this many versions of every file stay, however old.'],
        versions_days: [
          'Days versions are kept',
          'All versions from these days stay too, however many. Only a version that is neither among the latest nor this recent is removed; both 0 turn versions off.',
        ],
      },
    },
    plugins: {
      title: 'Plugins',
      fields: {
        workers: [
          'Background jobs at a time',
          'Batches of plugin jobs that run in parallel on the whole server; 0 means half the processor cores. Takes effect after a restart.',
        ],
        max_memory: [
          'Most memory of one plugin',
          'A multiple of 64 MiB: 1, 2, 4, 8, 16, 32 or 64. No plugin gets more, whatever its manifest asks for.',
        ],
        proxy_connections: [
          'Connections to plugin services',
          'WebSocket connections at a time to services plugins show through the server (an office editor), on the whole server.',
        ],
        proxy_connections_per_user: ['Connections to services per user', 'An open document takes one.'],
        proxy_requests_per_user: [
          'Requests to services per user',
          'Other requests to plugin services at a time from one user; more get "too many requests".',
        ],
        proxy_body_mib: ['Largest upload to a service', 'The most one request to a plugin service may carry.'],
      },
    },
    media: {
      title: 'Media (use.ffmpeg)',
      fields: {
        enabled: ['Plugins may use ffmpeg', 'Only those the administrator approved use.ffmpeg for.'],
        workers: ['ffmpeg processes at a time', '0 means half the processor cores. Takes effect after a restart.'],
        memory_mib: ['Memory per process', ''],
        time_seconds: ['Time per operation', ''],
        max_megapixels: ['Largest picture', 'Larger photos and video frames are refused before any work.'],
      },
    },
    onnx: {
      title: 'Neural networks (use.onnx)',
      fields: {
        enabled: [
          'Plugins may run neural networks',
          'Only those the administrator approved use.onnx for. Takes effect after a restart.',
        ],
        workers: [
          'Models loaded at once',
          'Each in a process of its own; one more unloads the one unused the longest.',
        ],
        memory_mib: ['Memory per model', 'The model and its work; must exceed the largest model.'],
        time_seconds: ['Time per run', 'Also the longest a model may take to load.'],
        keep_loaded_seconds: [
          'Keep a model loaded',
          'After its last run, so a batch of files loads it once. 0 unloads it right away.',
        ],
        max_model_mib: ['Largest model', ''],
      },
    },
    journal: {
      title: 'Journal',
      fields: {
        keep_days: ['Keep entries for', 'Older entries are removed automatically.'],
      },
    },
  } as Record<string, { title: string; fields: Record<string, [string, string]> }>,

  errors: {
    BAD_STATUS: 'the plugin answered something other than a list of status lines',
    INVALID_CREDENTIALS: 'Wrong username or password.',
    TOO_MANY_ATTEMPTS: 'Too many failed attempts. Try again later.',
    TOO_MANY_APP_PASSWORDS: 'Too many app passwords. Revoke an unused one first.',
    USER_NOT_FOUND: 'There is no such user.',
    SHARE_NOT_FOUND: 'This share has already ended.',
    VERSION_NOT_FOUND: 'This version no longer exists.',
    FILE_LOCKED: 'The file is being edited in an app; try again when it is closed.',
    FILE_CHANGED: 'The file has changed meanwhile; reload it and try again.',
    PLUGIN_REQUIRED: 'This plugin is part of the server and can’t be disabled.',
    PLUGIN_CANT_RUN: 'This plugin can’t run; see why in its row.',
    PLUGIN_EXISTS: 'A plugin with this id comes with the server.',
    PLUGIN_NOT_REMOVABLE: 'This plugin comes with the server; disable it instead.',
    ACCOUNT_DISABLED: 'This account is disabled.',
    USERNAME_TAKEN: 'This username is taken.',
    INVALID_USERNAME: 'Username: 3–32 characters, letters, digits, “.”, “_” or “-”.',
    WEAK_PASSWORD: 'The password must be at least 8 characters.',
    WRONG_PASSWORD: 'The current password is wrong.',
    NAME_TAKEN: 'An item with this name already exists here.',
    INVALID_NAME: 'This name is not allowed.',
    QUOTA_EXCEEDED: 'Not enough storage space.',
    STORAGE_FULL: 'The server’s disk is full. Ask the administrator.',
    SERVER_BUSY: 'The server is busy. Try again in a moment.',
    TOO_MANY_GRANTS: 'This app has too many accesses already; end some first.',
    FILE_TOO_LARGE: 'The file is larger than the upload limit.',
    MOVE_INTO_ITSELF: 'A folder cannot be moved into itself.',
    COPY_INTO_ITSELF: 'A folder cannot be copied into itself.',
    NETWORK: 'Network error. Check your connection.',
    NOT_ENOUGH_SPACE: 'Not enough free space for the test.',
  } as Record<string, string>,
};

type Texts = typeof en;

const ru: Texts = {
  appName: 'RustCloud',
  loading: 'Загрузка…',
  save: 'Сохранить',
  cancel: 'Отмена',
  close: 'Закрыть',
  create: 'Создать',
  delete: 'Удалить',
  copy: 'Копировать',
  saved: 'Сохранено',
  never: 'Никогда',
  unlimited: 'Без ограничений',

  login: 'Войти',
  logout: 'Выйти',
  username: 'Имя пользователя',
  password: 'Пароль',
  displayName: 'Отображаемое имя',
  email: 'Email',
  createAccount: 'Создать аккаунт',
  noAccount: 'Ещё нет аккаунта?',
  haveAccount: 'Уже есть аккаунт?',
  setupTitle: 'Добро пожаловать в RustCloud',
  setupHint: 'Создайте первый аккаунт — он станет администратором.',

  apps: 'Приложения',
  files: 'Файлы',
  myFiles: 'Мои файлы',
  links: 'Публичные ссылки',
  settings: 'Настройки',
  administration: 'Администрирование',
  collapse: 'Свернуть',
  expand: 'Развернуть',
  searchPlaceholder: 'Поиск файлов…',

  home: 'Главная',
  upload: 'Загрузить',
  uploadFolder: 'Загрузить папку',
  newFolder: 'Новая папка',
  newItem: 'Создать',
  newFileTitle: (kind: string) => `Создать: ${kind}`,
  textFile: 'Текстовый файл',
  textFileName: 'Новый текстовый файл',
  markdownFile: 'Текст Markdown',
  markdownFileName: 'Заметки',
  folderName: 'Имя папки',
  name: 'Имя',
  size: 'Размер',
  modified: 'Изменён',
  emptyFolder: 'Папка пуста. Перетащите сюда файлы или папки или нажмите «Загрузить».',
  dropHere: 'Отпустите файлы или папки для загрузки',
  dropInto: (n: number, folder: string) => `Объектов: ${n} → «${folder}»`,
  uploadWaiting: 'В очереди',
  download: 'Скачать',
  rename: 'Переименовать',
  move: 'Переместить',
  share: 'Поделиться',
  moveTo: 'Переместить в',
  copyTo: 'Копировать в',
  chooseFolder: 'Выбрать эту папку',
  up: 'Вверх',
  selected: (n: number) => `Выбрано: ${n}`,
  deleteSelected: 'Удалить выбранное',
  confirmDelete: (name: string) => `Удалить «${name}» навсегда? Это нельзя отменить.`,
  confirmDeleteMany: (n: number) => `Удалить объекты (${n}) навсегда? Это нельзя отменить.`,
  trash: 'Корзина',
  restore: 'Восстановить',
  deleteForever: 'Удалить навсегда',
  versions: 'Версии',
  lockedBy: (app: string) => `Редактируется в «${app}»: пока менять файл может только оно`,
  unlock: 'Снять блокировку',
  confirmUnlock: (app: string) =>
    `Снять блокировку? «${app}» может потерять несохранённые изменения. Делайте это, только если редактор уже закрыт.`,
  currentVersion: 'Текущая',
  noVersions: 'Прежних версий пока нет: когда файл заменяют, старое содержимое остаётся здесь.',
  versionsKept: (keep: number, days: number) =>
    keep && days
      ? `Хранятся последние ${ruCount(keep, 'версия', 'версии', 'версий')} и все версии за ${ruCount(days, 'день', 'дня', 'дней')}.`
      : keep
        ? `Хранятся последние ${ruCount(keep, 'версия', 'версии', 'версий')}.`
        : days
          ? `Хранятся версии за ${ruCount(days, 'день', 'дня', 'дней')}.`
          : 'Администратор отключил версии.',
  versionsQuota: 'Версии занимают место в вашем хранилище; когда оно кончается, первыми удаляются самые старые.',
  emptyTrash: 'Очистить корзину',
  confirmEmptyTrash: 'Удалить всё из корзины навсегда? Это нельзя отменить.',
  trashEmpty: 'Корзина пуста.',
  trashHint:
    'Удалённые файлы и папки лежат здесь, пока вы не удалите их навсегда. Администратор может задать срок хранения.',
  confirmOverwrite: (name: string) => `«${name}» уже существует. Заменить?`,
  uploads: 'Загрузки',
  uploadDone: 'Готово',
  uploadFailed: 'Ошибка',
  searchResults: (query: string) => `Поиск: «${query}»`,
  nothingFound: 'Ничего не найдено.',
  openFolder: 'Открыть папку',
  previous: 'Предыдущий',
  next: 'Следующий',
  noPreview: 'Для этого типа файлов нет предпросмотра.',
  previewFailed: 'Не удалось загрузить предпросмотр.',
  previewTruncated: (size: string) => `Показаны первые ${size}. Скачайте файл, чтобы увидеть его целиком.`,

  createLink: 'Создать ссылку',
  linkExpires: 'Действует до',
  linkExpiresHint: 'Оставьте пустым, чтобы ссылка не истекала.',
  copyLink: 'Скопировать ссылку',
  linkCopied: 'Ссылка скопирована',
  deleteLink: 'Удалить ссылку',
  noLinks: 'Вы пока ничем не поделились.',
  sharedItem: 'Объект',
  existingLinks: 'Созданные ссылки',

  sharedWithYou: 'С вами поделились',
  linkGone: 'Ссылка не существует или истекла.',
  downloadFile: 'Скачать файл',

  profile: 'Профиль',
  changePassword: 'Смена пароля',
  currentPassword: 'Текущий пароль',
  newPassword: 'Новый пароль',
  passwordChanged: 'Пароль изменён. Остальные сеансы завершены.',
  storage: 'Хранилище',
  usedOf: (used: string, total: string) => `Занято ${used} из ${total}`,
  usedNoLimit: (used: string) => `Занято ${used}, без ограничений`,
  versionsUse: (size: string) => `Из них прежние версии файлов: ${size}.`,
  display: 'Отображение',
  showHidden: 'Показывать скрытые файлы',
  showHiddenHint:
    'Файлы и папки, имя которых начинается с точки, например данные приложений (.appdata). Показываются полупрозрачными.',
  appAccess: 'Приложения с доступом',
  appAccessNone: 'Ни у одного приложения нет доступа к вашим файлам.',
  accessRead: 'чтение',
  accessWrite: 'чтение и изменение',
  accessWhileOpen: 'пока открыт',
  openIn: (app: string) => `Открыть в «${app}»`,
  openFileTitle: 'Открыть файл в приложении?',
  openFileText: (app: string, file: string, write: boolean) =>
    `«${app}» сможет ${write ? 'читать и изменять' : 'читать'} «${file}», пока он открыт, — и больше ничего.`,
  rememberForApp: 'Больше не спрашивать для этого приложения в этом браузере',
  open: 'Открыть',
  appCannotOpen: 'Это приложение не открывает такие файлы.',
  accessStanding: 'до отзыва',
  accessSince: (date: string) => `с ${date}`,
  revoke: 'Отозвать',
  accessRequestTitle: (app: string) => `«${app}» просит папку`,
  accessRequestText: (app: string) =>
    `«${app}» просит доступ к одной папке ваших файлов — до тех пор, пока вы его не отзовёте. Остальные файлы приложение не увидит.`,
  accessCreateFolder: (name: string) => `Создать папку «${name}»`,
  accessPickFolder: 'Выбрать существующую папку…',
  accessPickedFolder: (name: string) => `Папка «${name}»`,
  accessPickAnother: 'Выбрать другую…',
  accessPickTitle: 'Папка для приложения',
  accessWillWrite: 'Приложение сможет читать, изменять и удалять файлы в этой папке.',
  accessWillRead: 'Приложение сможет читать файлы в этой папке, но не изменять их.',
  accessAllow: 'Разрешить',
  appPasswords: 'Пароли приложений',
  appPasswordsHint:
    'Для календарей и других программ, которые спрашивают логин и пароль. У каждой программы свой пароль и только нужный ей доступ; пароль от аккаунта остаётся при вас.',
  appPasswordNew: 'Новый пароль приложения',
  appPasswordName: 'Название',
  appPasswordNamePlaceholder: 'Календарь на телефоне',
  appPasswordNoAccess: 'нет доступа',
  appPasswordChooseApp: 'Выберите хотя бы одно приложение.',
  appPasswordCreated: 'Скопируйте пароль сейчас: больше он не покажется. Входите с вашим логином и этим паролем.',
  appPasswordCopied: 'Скопировано',
  appPasswordUsed: (date: string) => `использовался ${date}`,
  appPasswordNeverUsed: 'ещё не использовался',
  appShares: 'Общий доступ к данным приложений',
  appSharesNone: 'Пока ничем не делились. Приложения вроде Календаря предлагают поделиться на своих страницах.',
  appSharesGiven: 'Вы поделились',
  appSharesReceived: 'С вами поделились',
  appShareWith: (label: string, who: string) => `«${label}» — с ${who}`,
  appShareFrom: (label: string, who: string) => `«${label}» от ${who}`,
  appShareLeave: 'Убрать',
  appShareTitle: (label: string) => `Поделиться: «${label}»`,
  appShareText: (app: string) =>
    `Добавленные пользователи увидят это в своём приложении «${app}» и в подключённых к нему программах. Закрыть доступ можно в любой момент.`,
  appShareUsername: 'Логин',
  appShareAdd: 'Поделиться',
  appShareNobody: 'Пока ни с кем не поделились.',
  appShareCanRead: 'просмотр',
  appShareCanWrite: 'правка',
  pluginsTitle: 'Плагины сервера',
  pluginSource: { built_in: 'часть сервера', shipped: 'поставляется с сервером', installed: 'установлен' },
  pluginState: { running: 'Работает', disabled: 'Отключён', failed: 'Не может работать' },
  pluginEnable: 'Включить',
  pluginDisable: 'Отключить',
  pluginResources: (
    e: { memory_mib: number; data_mib: number; time_seconds: number; scale: { workers: number } },
    own: boolean,
  ) =>
    `Получает ${sizeRu(e.memory_mib)} памяти, файлы до ${sizeRu(e.data_mib)} за раз, ${e.time_seconds} с работы, ` +
    `порций задач одновременно: ${e.scale.workers}` +
    (own ? ' (задано администратором)' : ''),
  pluginAsks: (s: { memory: number; time: number; workers: number }) =>
    `просит: память ×${s.memory}, время ×${s.time}, порций одновременно: ${s.workers}`,
  pluginAdvanced: 'Расширенные настройки',
  pluginMemory: 'Память',
  pluginMemoryStep: (step: number) => `${sizeRu(64 * step)}, файлы до ${sizeRu(16 * step)}`,
  pluginTime: 'Время',
  pluginTimeStep: (step: number) => `${10 * step} с работы, ${60 * step} с всего`,
  pluginWorkers: 'Порций задач одновременно',
  pluginAsManifest: 'Как в манифесте',
  pluginAdvancedHint: 'Остальное считается из них: чтение файлов за раз, тело запроса, общее время.',
  pluginsJobsMemory: (mib: number) => `Фоновые задачи плагинов занимают одновременно до ${sizeRu(mib)} памяти.`,
  pluginFiles: {
    none: 'без доступа к файлам',
    opened: 'файлы, которые пользователь в нём открыл',
    folder: 'папки, которые даст пользователь',
    all: 'все файлы, которые даст пользователь',
  },
  pluginFilesWrite: 'может их менять',
  pluginAddresses: 'Адреса',
  pluginNetwork: 'Сеть',
  pluginUses: 'Возможности',
  pluginFrames: 'Показывает сервисы',
  pluginProxy: (paths: string[], target: string) =>
    `Показывает сервис ${target} по путям ${paths.join(', ')} этого сервера: он действует от имени пользователей, открывших в плагине файлы, — утверждайте, только если доверяете ему`,
  ffmpegRuns: (version: string, layers: string) => `use.ffmpeg: ffmpeg ${version}; защита — ${layers}.`,
  ffmpegMissing: (why: string) => `use.ffmpeg недоступен: ${why}`,
  onnxRuns: (engine: string, minutes: number) =>
    `use.onnx: ${engine}; модель остаётся в памяти ${minutes} мин после последнего вызова.`,
  onnxLoaded: (models: string[]) => (models.length ? `В памяти: ${models.join(', ')}.` : 'Моделей в памяти нет.'),
  onnxMissing: (why: string) => `use.onnx недоступен: ${why}`,
  pluginEnableNote:
    'Включая плагин, вы утверждаете, что он может просить у пользователей и к каким серверам обращаться.',
  pluginInstall: 'Установить плагин…',
  pluginInstallHint:
    'Архив .zip с manifest.toml и app.wasm. Новый плагин ставится отключённым; обновление остаётся включённым, если не просит больше прав.',
  pluginInstalled: (name: string, version: string) => `Установлен ${name} ${version}.`,
  pluginRemove: 'Удалить',
  pluginSettings: 'Настройки',
  pluginConfigure: 'Настроить',
  pluginTabSettings: 'Настройки',
  pluginTabStatus: 'Состояние',
  pluginTabSystem: 'Системные',
  pluginNoSettings: 'У плагина нет своих настроек.',
  pluginReport: 'Что сообщает плагин',
  pluginReportFailed: (why: string) => `Плагин не ответил: ${why}`,
  pluginReportEmpty: 'Сообщать нечего.',
  pluginRights: 'Что ему можно',
  pluginSecretSet: 'сохранён; введите, чтобы заменить',
  pluginSecretClear: 'Очистить',
  pluginSettingsSaved: 'Сохранено.',
  pluginConfirmRemove: (name: string) =>
    `Удалить плагин «${name}» вместе с его хранилищем? Данные пользователей в нём останутся и вернутся, если установить его снова.`,
  pluginStorage: (size: string, files: number) => `Хранилище: ${size}, файлов: ${files}`,
  pluginStorageFiles: 'Файлы',
  pluginStorageClear: 'Очистить',
  pluginConfirmClear: (name: string) =>
    `Очистить хранилище «${name}»? Файлы, которые плагин скачивает, загрузятся снова; остальное пропадёт.`,
  pluginDownloading: (path: string, progress: string, left: number) =>
    `Загрузка ${path}: ${progress}${left > 1 ? `; осталось файлов: ${left}` : ''}`,
  pluginDownloadFailed: (path: string, error: string) => `Не удалось скачать ${path}: ${error}`,
  pluginRetry: 'Повторить',
  pluginJobs: 'Фоновые задачи',
  pluginJobsLeft: (waiting: number, failed: number) => `осталось: ${waiting}, ошибок: ${failed}`,
  pluginJobsBatch: (batch: number) => `по ${batch} в порции`,
  done: 'Готово',

  users: 'Пользователи',
  serverTab: 'Сервер',
  systemTitle: 'Сервер и система',
  systemVersion: 'Версия',
  systemUptime: 'Работает',
  systemOs: 'Система',
  systemKernel: 'ядро',
  systemContainer: 'в контейнере',
  systemCpu: 'Процессор',
  systemCpus: (n: number) => ruCount(n, 'ядро', 'ядра', 'ядер'),
  systemLoad: (load: string) => `нагрузка ${load} (1 / 5 / 15 мин)`,
  systemMemory: 'Память',
  systemSwap: (usage: string) => `Подкачка: ${usage}`,
  systemContainerMemory: 'Лимит контейнера',
  systemProcess: 'Процесс RustCloud',
  systemProcessMemory: (size: string) => `${size} памяти`,
  systemThreads: (n: number) => ruCount(n, 'поток', 'потока', 'потоков'),
  systemDatabase: 'База данных',
  duration: (seconds: number) => {
    const days = Math.floor(seconds / 86400);
    const hours = Math.floor((seconds % 86400) / 3600);
    const minutes = Math.floor((seconds % 3600) / 60);
    return days ? `${days} д ${hours} ч` : hours ? `${hours} ч ${minutes} мин` : `${minutes} мин`;
  },
  newUser: 'Новый пользователь',
  isAdmin: 'Администратор',
  disabled: 'Отключён',
  quotaMib: 'Квота, МиБ',
  created: 'Создан',
  resetPassword: 'Сбросить пароль',
  newPasswordFor: (name: string) => `Новый пароль для ${name}:`,
  you: 'вы',

  storageTitle: 'Хранилище',
  storageKinds: { local: 'Локальный диск' },
  diskUsage: (used: string, total: string, percent: string) => `Занято ${used} из ${total} (${percent}%)`,
  diskFree: (free: string) => `свободно ${free}`,
  storedFiles: (size: string, count: number) => `Файлы RustCloud: ${size}, файлов: ${count}`,
  nearlyFull: (percent: string) => `Диск хранилища заполнен на ${percent}%. Освободите место или расширьте диск.`,
  speedTest: 'Скорость',
  speedResult: (write: string, read: string) => `запись ${write} МиБ/с, чтение ${read} МиБ/с`,
  speedTestedAt: (when: string, size: string) => `замер ${when}, объём ${size}`,
  speedTestRun: 'Проверить скорость',
  speedTestRunning: 'Проверяю…',
  speedNever: 'Ещё не измерялась.',

  journalProblems: 'Только предупреждения и ошибки',
  journalSearch: 'Поиск: пользователь, файл, адрес…',
  journalRefresh: 'Обновить',
  journalMore: 'Показать старые',
  journalEmpty: 'Записей нет.',
  journalTime: 'Время',
  journalUser: 'Пользователь',
  journalAddress: 'Адрес',
  journalEvent: 'Событие',
  journalDropped: (n: number) => `Потеряно событий: ${n} — журнал не успевал их записывать.`,
  journalActions: {
    'server.started': () => 'Сервер запущен',
    'auth.login': () => 'Вход',
    'auth.login_failed': () => 'Неудачный вход',
    'auth.logout': () => 'Выход',
    'auth.password_change': () => 'Сменил свой пароль',
    'account.create': (d) => `Создан аккаунт «${d.account}»`,
    'account.update': (d) => `Изменён аккаунт «${d.account}»`,
    'folder.create': (d) => `Создал папку «${d.name}»`,
    'file.upload': (d) => `Загрузил «${d.name}»`,
    'file.version.download': (d) => `Скачана прежняя версия «${d.name}»`,
    'file.version.restore': (d) => `Восстановлена прежняя версия «${d.name}»`,
    'file.download': (d) => `Скачал «${d.name}»`,
    'file.rename': (d) => `Переименовал «${d.name}» в «${d.new_name}»`,
    'file.move': (d) => `Переместил «${d.name}»`,
    'file.copy': (d) => `Скопировал «${d.name}»`,
    'file.delete': (d) => `Удалил «${d.name}»`,
    'share.create': (d) => `Создал публичную ссылку на «${d.name}»`,
    'share.delete': () => 'Удалил публичную ссылку',
    'share.download': (d) => `«${d.name}» скачан по публичной ссылке`,
    'settings.save': (d) => `Сохранены настройки: ${d.section}`,
    'grant.create': (d) => `Дал приложению «${d.app}» доступ к «${d.name}»`,
    'grant.revoke': (d) => `Доступ приложения «${d.app}» завершён`,
    'app_password.create': (d) => `Создал пароль приложения «${d.name}» (${d.scopes})`,
    'app_password.revoke': (d) => `Отозвал пароль приложения «${d.name}»`,
    'app_share.create': (d) =>
      `Поделился «${d.name}» приложения «${d.app}» с ${d.with} (${String(d.write) === 'true' ? 'правка' : 'просмотр'})`,
    'app_share.revoke': (d) =>
      d.by === 'owner'
        ? `Закрыл доступ к «${d.name}» приложения «${d.app}»`
        : `Убрал чужой «${d.name}» приложения «${d.app}»`,
    'storage.speed_test': (d) => `Проверка скорости хранилища: запись ${d.write} МиБ/с, чтение ${d.read} МиБ/с`,
  },

  settingFromEnv: 'Задано переменной окружения — меняется там.',
  units: {
    days: 'дней',
    MiB: 'МиБ',
    attempts: 'попыток',
    s: 'с',
    batches: 'порций',
    processes: 'процессов',
    MP: 'Мпикс',
    x64MiB: '× 64 МиБ',
    versions: 'версий',
    connections: 'соединений',
    requests: 'запросов',
  },
  sections: {
    server: {
      title: 'Настройки',
      fields: {
        public_url: [
          'Публичный адрес',
          'Адрес, который открывают пользователи, например https://cloud.example.com. От него строятся публичные ссылки (если пусто — от текущего адреса в браузере). С https:// cookie передаются только по HTTPS.',
        ],
        other_urls: ['Другие адреса', 'По одному на строку, например адрес в LAN: http://192.168.1.10:8080'],
        only_listed_urls: [
          'Отвечать только на эти адреса',
          'Запросы на другие имена хостов отклоняются (localhost работает всегда). Защита от DNS rebinding.',
        ],
        cors_origins: [
          'Сайты с доступом к API (CORS)',
          'По одному на строку, например https://app.example.com. Они входят по токену, cookie им не передаются.',
        ],
        trusted_proxies: [
          'Доверенные reverse proxy',
          'Адреса или сети по одной на строку (например 172.16.0.0/12). Их X-Forwarded-For и X-Forwarded-Proto считаются верными.',
        ],
      },
    },
    accounts: {
      title: 'Аккаунты',
      fields: {
        open_registration: ['Кто угодно может создать аккаунт', 'Иначе аккаунты создают только администраторы.'],
        session_days: ['Вход действует', ''],
        default_quota_mib: [
          'Квота для новых аккаунтов',
          'Пусто — без ограничений. У существующих аккаунтов квота не меняется.',
        ],
        max_login_failures: [
          'Неудачных входов до блокировки на 15 минут',
          'На аккаунт; с одного IP-адреса — в пять раз больше.',
        ],
      },
    },
    files: {
      title: 'Файлы',
      fields: {
        max_upload_mib: ['Максимальный размер загрузки', 'Пусто — без ограничений.'],
        trash_days: ['Дней в корзине', 'Потом удалённое исчезает навсегда; 0 — до очистки корзины.'],
        versions_keep: [
          'Версий хранить в любом случае',
          'Последние столько версий каждого файла остаются, какими бы старыми ни были.',
        ],
        versions_days: [
          'Дней хранить версии',
          'Все версии за эти дни тоже остаются, сколько бы их ни было. Удаляется только версия, которая не входит ни в последние, ни в эти дни; оба 0 — версии отключены.',
        ],
      },
    },
    plugins: {
      title: 'Плагины',
      fields: {
        workers: [
          'Фоновых задач одновременно',
          'Сколько порций задач плагинов выполняется параллельно на всём сервере; 0 — половина ядер процессора. Применяется после перезапуска.',
        ],
        max_memory: [
          'Наибольшая память плагина',
          'Кратно 64 МБ: 1, 2, 4, 8, 16, 32 или 64. Больше не получит ни один плагин, что бы ни просил манифест.',
        ],
        proxy_connections: [
          'Соединений с сервисами плагинов',
          'Сколько WebSocket-соединений одновременно на всём сервере с сервисами, которые плагины показывают через сервер (офисный редактор).',
        ],
        proxy_connections_per_user: ['Соединений с сервисами на пользователя', 'Открытый документ занимает одно.'],
        proxy_requests_per_user: [
          'Запросов к сервисам на пользователя',
          'Сколько остальных запросов к сервисам плагинов одновременно от одного пользователя; лишние получают «слишком много запросов».',
        ],
        proxy_body_mib: ['Наибольшая загрузка в сервис', 'Сколько может нести один запрос к сервису плагина.'],
      },
    },
    media: {
      title: 'Медиа (use.ffmpeg)',
      fields: {
        enabled: ['Плагины могут использовать ffmpeg', 'Только те, кому администратор утвердил use.ffmpeg.'],
        workers: ['Процессов ffmpeg одновременно', '0 — половина ядер процессора. Применяется после перезапуска.'],
        memory_mib: ['Память на процесс', ''],
        time_seconds: ['Время на операцию', ''],
        max_megapixels: ['Самая большая картинка', 'Фото и кадры видео больше этого отклоняются до начала работы.'],
      },
    },
    onnx: {
      title: 'Нейросети (use.onnx)',
      fields: {
        enabled: [
          'Плагины могут запускать нейросети',
          'Только те, кому администратор утвердил use.onnx. Применяется после перезапуска.',
        ],
        workers: [
          'Моделей в памяти одновременно',
          'Каждая в своём процессе; новая выгружает ту, что дольше всех не использовалась.',
        ],
        memory_mib: ['Память на модель', 'Модель и её работа; должна быть больше самой большой модели.'],
        time_seconds: ['Время на вызов', 'И наибольшее время загрузки модели.'],
        keep_loaded_seconds: [
          'Держать модель в памяти',
          'После последнего вызова, чтобы порция файлов загружала её один раз. 0 — выгружать сразу.',
        ],
        max_model_mib: ['Самая большая модель', ''],
      },
    },
    journal: {
      title: 'Журнал',
      fields: {
        keep_days: ['Хранить записи', 'Более старые записи удаляются автоматически.'],
      },
    },
  },

  errors: {
    BAD_STATUS: 'плагин ответил не списком строк состояния',
    INVALID_CREDENTIALS: 'Неверное имя пользователя или пароль.',
    TOO_MANY_ATTEMPTS: 'Слишком много неудачных попыток. Попробуйте позже.',
    TOO_MANY_APP_PASSWORDS: 'Слишком много паролей приложений. Сначала отзовите ненужный.',
    USER_NOT_FOUND: 'Такого пользователя нет.',
    SHARE_NOT_FOUND: 'Этот доступ уже закрыт.',
    VERSION_NOT_FOUND: 'Этой версии уже нет.',
    FILE_LOCKED: 'Файл сейчас редактируется в приложении; повторите, когда его закроют.',
    FILE_CHANGED: 'Файл тем временем изменился; обновите его и повторите.',
    PLUGIN_REQUIRED: 'Этот плагин — часть сервера, его нельзя отключить.',
    PLUGIN_CANT_RUN: 'Этот плагин не может работать — причина в его строке.',
    PLUGIN_EXISTS: 'Плагин с таким id поставляется с сервером.',
    PLUGIN_NOT_REMOVABLE: 'Этот плагин поставляется с сервером — отключите его.',
    ACCOUNT_DISABLED: 'Аккаунт отключён.',
    USERNAME_TAKEN: 'Это имя пользователя занято.',
    INVALID_USERNAME: 'Имя пользователя: 3–32 символа, латинские буквы, цифры, «.», «_» или «-».',
    WEAK_PASSWORD: 'Пароль должен быть не короче 8 символов.',
    WRONG_PASSWORD: 'Текущий пароль неверен.',
    NAME_TAKEN: 'Здесь уже есть объект с таким именем.',
    INVALID_NAME: 'Такое имя недопустимо.',
    QUOTA_EXCEEDED: 'Недостаточно места в хранилище.',
    STORAGE_FULL: 'Диск сервера заполнен. Обратитесь к администратору.',
    SERVER_BUSY: 'Сервер занят. Попробуйте через минуту.',
    TOO_MANY_GRANTS: 'У приложения уже слишком много доступов; сначала завершите лишние.',
    FILE_TOO_LARGE: 'Файл больше разрешённого размера загрузки.',
    MOVE_INTO_ITSELF: 'Папку нельзя переместить в саму себя.',
    COPY_INTO_ITSELF: 'Папку нельзя скопировать в саму себя.',
    NETWORK: 'Ошибка сети. Проверьте подключение.',
    NOT_ENOUGH_SPACE: 'Недостаточно свободного места для проверки.',
  },
};

export const locale = navigator.language.toLowerCase().startsWith('ru') ? 'ru' : 'en';

/** Texts in the user's language. */
export const t: Texts = locale === 'ru' ? ru : en;

/** Title of a settings section; the raw name for sections without texts. */
export function sectionTitle(section: string): string {
  return t.sections[section]?.title ?? section;
}

export function settingLabel(section: string, key: string): string {
  return t.sections[section]?.fields[key]?.[0] ?? key;
}

export function settingHint(section: string, key: string): string {
  return t.sections[section]?.fields[key]?.[1] ?? '';
}

/** What a journal entry says, in the user's language when its action is known. */
export function journalText(entry: { action: string | null; message: string; details: Record<string, unknown> }) {
  const describe = entry.action ? t.journalActions[entry.action] : undefined;
  return describe ? describe(entry.details) : entry.message;
}

/** A readable message for an error from the API (or anything thrown). */
export function errorText(error: unknown): string {
  if (error && typeof error === 'object' && 'code' in error && typeof error.code === 'string') {
    const known = t.errors[error.code];
    if (known) return known;
  }
  return error instanceof Error ? error.message : String(error);
}
