// The files panel's "New" menu: kinds of files the server offers itself and
// the ones apps add with `[[new_files]]` in their manifests.

import type { AppManifest } from './api';

export interface NewFileKind {
  /** Unique in the menu: `core:txt`, `rc-office:docx`. */
  key: string;
  /** What the menu says. */
  label: string;
  /** The new file's name before the extension. */
  baseName: string;
  /** Without the dot. */
  extension: string;
  /** The app whose template gives the content; none: the file starts empty. */
  appId?: string;
  template?: string;
}

/** The server's own kinds: plain text and Markdown. */
export function coreKinds(texts: { textFile: string; textFileName: string; markdownFile: string; markdownFileName: string }) {
  return [
    { key: 'core:txt', label: texts.textFile, baseName: texts.textFileName, extension: 'txt' },
    { key: 'core:md', label: texts.markdownFile, baseName: texts.markdownFileName, extension: 'md' },
  ] satisfies NewFileKind[];
}

/** The kinds apps add, in the order of the apps, labelled in `locale` (English if missing). */
export function appKinds(apps: AppManifest[], locale: string): NewFileKind[] {
  return apps.flatMap((app) =>
    (app.new_files ?? []).map((kind) => {
      const label = kind.label[locale] ?? kind.label.en ?? kind.extension;
      return {
        key: `${app.id}:${kind.extension}`,
        label,
        baseName: label,
        extension: kind.extension,
        appId: app.id,
        template: kind.template,
      };
    }),
  );
}

/** `base.ext`, or `base (2).ext`, `base (3).ext`… — the first name not in `taken` (case-insensitive). */
export function freeName(base: string, extension: string, taken: string[]): string {
  const names = new Set(taken.map((name) => name.toLowerCase()));
  for (let n = 1; ; n++) {
    const name = `${base}${n > 1 ? ` (${n})` : ''}.${extension}`;
    if (!names.has(name.toLowerCase())) return name;
  }
}
