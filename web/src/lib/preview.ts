// What kind of preview a file gets. Anything else is downloaded.

import type { FileNode } from './api';

export type PreviewKind = 'image' | 'video' | 'audio' | 'text';

/** Images browsers show safely (no SVG: it can carry scripts). */
const IMAGE_TYPES = ['image/png', 'image/jpeg', 'image/gif', 'image/webp', 'image/avif', 'image/bmp'];

/** Text formats that don't start with `text/`. */
const TEXT_TYPES = [
  'application/json',
  'application/xml',
  'application/javascript',
  'application/x-sh',
  'application/toml',
  'application/yaml',
  'application/x-yaml',
  'application/sql',
];

/** Extensions shown as text whatever type the server guessed (often application/octet-stream). */
const TEXT_EXTENSIONS = new Set(
  (
    'txt md markdown log csv tsv json jsonl xml yml yaml toml ini cfg conf env properties sh bash zsh fish ps1 bat ' +
    'rs go py rb php java kt kts scala swift c h cc cpp hpp cs js mjs cjs ts tsx jsx vue svelte css scss less html htm ' +
    'sql graphql proto lua pl r dart ex exs erl hs ml clj nix tf dockerfile makefile gitignore editorconfig'
  ).split(' '),
);

/** Text previews show at most this many bytes. */
export const TEXT_PREVIEW_BYTES = 512 * 1024;

export function previewKind(node: FileNode): PreviewKind | null {
  if (node.is_dir) return null;
  const mime = node.mime_type ?? '';
  if (IMAGE_TYPES.includes(mime)) return 'image';
  if (mime.startsWith('video/')) return 'video';
  if (mime.startsWith('audio/')) return 'audio';
  if (mime.startsWith('text/') || TEXT_TYPES.includes(mime)) return 'text';

  const name = node.name.toLowerCase();
  const extension = name.includes('.') ? name.slice(name.lastIndexOf('.') + 1) : name;
  return TEXT_EXTENSIONS.has(extension) ? 'text' : null;
}
