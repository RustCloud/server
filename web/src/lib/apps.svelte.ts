// The apps of the server, loaded after login; the side panel lists them.

import { api, type AppManifest, type FileNode } from './api';
import { openerPath, sidebarEntries, type AppEntry, type Opener } from './apps';
import { locale } from './i18n';
import { go } from './router.svelte';

export const apps: {
  entries: AppEntry[];
  manifests: AppManifest[];
  limits: Record<string, { data_bytes: number }>;
  frames: Record<string, string[]>;
  proxies: Record<string, string[]>;
} = $state({
  entries: [],
  manifests: [],
  limits: {},
  frames: {},
  proxies: {},
});

/**
 * The file the user just chose to open from the files list: the app's page
 * opens it without asking again, and closing it goes back to the list.
 */
let chosenOpen: { appId: string; fileId: string } | null = null;

/** Opens a file in an app, as the user chose in the files list. */
export function openInApp(opener: Opener, node: FileNode) {
  chosenOpen = { appId: opener.appId, fileId: node.id };
  go(openerPath(opener, node.id));
}

/** Whether the user just chose to open this file in this app (asked once). */
export function takeChosen(appId: string, fileId: string): boolean {
  const yes = chosenOpen?.appId === appId && chosenOpen.fileId === fileId;
  chosenOpen = null;
  return yes;
}

export async function loadApps() {
  try {
    const list = await api.apps();
    apps.entries = sidebarEntries(list.apps, locale);
    apps.manifests = list.apps;
    apps.limits = list.limits ?? {};
    apps.frames = list.frames ?? {};
    apps.proxies = list.proxies ?? {};
  } catch {
    // Keep what is shown; the next login tries again.
  }
}
