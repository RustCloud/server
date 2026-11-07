// The apps in the side panel, built from the server's list of apps (GET /api/v1/apps),
// so a new app appears without changes here.

import type { AppManifest, FileNode } from './api';

export interface AppEntry {
  id: string;
  /** Icon name from the manifest; may be unknown to this UI. */
  icon: string;
  title: string;
  /** Where the panel link leads. */
  href: string;
  /** Route prefixes that belong to the app (the link is highlighted on them), e.g. `['app', 'notes']`. */
  routes: string[][];
}

/** Whether the current route (its parts) belongs to the app. */
export function isActive(entry: AppEntry, parts: string[]): boolean {
  return entry.routes.some((prefix) => prefix.every((part, i) => parts[i] === part));
}

/** Side panel entries for the apps that have pages, titled in `locale` (English if missing). */
export function sidebarEntries(apps: AppManifest[], locale: string): AppEntry[] {
  return apps.flatMap((app) => {
    if (!app.ui) return [];
    const entryParts = app.ui.entry.split('/').filter(Boolean);
    return [
      {
        id: app.id,
        icon: app.ui.icon,
        title: app.name[locale] ?? app.name.en ?? app.id,
        href: app.ui.entry,
        routes: [entryParts, ...(app.ui.routes ?? []).map((route) => [route])],
      },
    ];
  });
}

/** An app that opens a file, for the files menu. */
export interface Opener {
  appId: string;
  /** The manifest's label, or the app's name. */
  label: string | null;
  appName: string;
  page: string;
  write: boolean;
  full: boolean;
}

/** Whether `mime` is one of `patterns` (`image/jpeg`, `image/*`). */
export function mimeMatches(patterns: string[], mime: string | null): boolean {
  if (!mime) return false;
  const type = mime.toLowerCase().split(';')[0].trim();
  return patterns.some(
    (pattern) => pattern === type || (pattern.endsWith('/*') && type.startsWith(pattern.slice(0, -1))),
  );
}

/** Whether a file's name has one of `extensions` (`env`), or is one (`dockerfile`). */
export function extensionMatches(extensions: string[] | undefined, name: string): boolean {
  if (!extensions?.length) return false;
  const lower = name.toLowerCase();
  const extension = lower.includes('.') ? lower.slice(lower.lastIndexOf('.') + 1) : lower;
  return extensions.includes(extension);
}

/** The apps that open `node`, by their `[[open_with]]` entries, in the order of the apps. */
export function openersFor(apps: AppManifest[], node: FileNode, locale: string): Opener[] {
  if (node.is_dir) return [];
  return apps.flatMap((app) =>
    (app.open_with ?? [])
      .filter((open) => mimeMatches(open.mime, node.mime_type) || extensionMatches(open.extensions, node.name))
      .map((open) => ({
        appId: app.id,
        label: open.label?.[locale] ?? open.label?.en ?? null,
        appName: app.name[locale] ?? app.name.en ?? app.id,
        page: open.page,
        write: open.write ?? false,
        full: (open.layout ?? 'full') === 'full',
      })),
  );
}

/** The panel path that opens `fileId` with an opener: `/app/<id><page>?file=<id>`. */
/**
 * The origin of a service the panel may show for an app's page at `url`, or
 * null: one the administrator approved (`frames`), or a path of the server
 * itself that goes to the app's service (`proxies`, on `ownOrigin`).
 */
export function serviceOrigin(
  url: string,
  frames: string[],
  proxies: string[],
  ownOrigin: string,
): string | null {
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    return null;
  }
  if (frames.includes(parsed.origin)) return parsed.origin;
  const proxied = proxies.some((prefix) => parsed.pathname === prefix || parsed.pathname.startsWith(prefix + '/'));
  return parsed.origin === ownOrigin && proxied ? ownOrigin : null;
}

export function openerPath(opener: Pick<Opener, 'appId' | 'page'>, fileId: string): string {
  return `/app/${encodeURIComponent(opener.appId)}${opener.page}?file=${encodeURIComponent(fileId)}`;
}
