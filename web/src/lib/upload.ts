// Collecting dropped or chosen folders for upload, and creating their folder tree.

import { api, ApiError, type FileNode } from './api';

/** Something to upload: a file, or an (empty) folder, at a path below the target folder. */
export interface UploadEntry {
  /** Folder names from the target folder down to where the entry goes. */
  folders: string[];
  /** `undefined` for a folder that only needs to exist. */
  file?: File;
}

/** Entries from a file input; with `webkitdirectory` files carry their relative path. */
export function entriesFromInput(files: FileList): UploadEntry[] {
  return Array.from(files, (file) => {
    const parts = (file.webkitRelativePath || file.name).split('/');
    return { folders: parts.slice(0, -1), file };
  });
}

/**
 * Entries from a drop, walking dropped folders. Must be called while the drop
 * event is being handled: browsers clear `dataTransfer` right after it.
 */
export function entriesFromDrop(data: DataTransfer): Promise<UploadEntry[]> {
  const roots = Array.from(data.items)
    .filter((item) => item.kind === 'file')
    .map((item) => item.webkitGetAsEntry());
  if (roots.some((entry) => entry === null)) {
    // No entry API (very old browser): plain files only.
    return Promise.resolve(Array.from(data.files, (file) => ({ folders: [], file })));
  }
  return Promise.all(roots.map((entry) => walk(entry!, []))).then((lists) => lists.flat());
}

async function walk(entry: FileSystemEntry, folders: string[]): Promise<UploadEntry[]> {
  if (entry.isFile) {
    const file = await new Promise<File>((resolve, reject) => (entry as FileSystemFileEntry).file(resolve, reject));
    return [{ folders, file }];
  }
  const inside = [...folders, entry.name];
  const children = await readAll((entry as FileSystemDirectoryEntry).createReader());
  if (children.length === 0) return [{ folders: inside }];
  const lists = await Promise.all(children.map((child) => walk(child, inside)));
  return lists.flat();
}

/** `readEntries` returns at most ~100 entries per call; read until it returns none. */
async function readAll(reader: FileSystemDirectoryReader): Promise<FileSystemEntry[]> {
  const all: FileSystemEntry[] = [];
  for (;;) {
    const batch = await new Promise<FileSystemEntry[]>((resolve, reject) => reader.readEntries(resolve, reject));
    if (batch.length === 0) return all;
    all.push(...batch);
  }
}

/**
 * Creates folders on demand and remembers them, so a folder upload creates
 * each folder once even when its files are uploaded in parallel. A folder
 * that already exists is reused.
 */
export class FolderMaker {
  private made = new Map<string, Promise<string>>();

  constructor(private readonly rootId: string) {}

  /** The id of the folder at `folders` below the root, creating what's missing. */
  folderId(folders: string[]): Promise<string> {
    if (folders.length === 0) return Promise.resolve(this.rootId);
    const key = folders.join('/');
    let id = this.made.get(key);
    if (!id) {
      id = this.folderId(folders.slice(0, -1)).then((parentId) => makeFolder(parentId, folders[folders.length - 1]));
      this.made.set(key, id);
    }
    return id;
  }
}

async function makeFolder(parentId: string, name: string): Promise<string> {
  try {
    return (await api.createFolder(parentId, name)).id;
  } catch (e) {
    if (!(e instanceof ApiError) || e.status !== 409) throw e;
    const existing = (await api.children(parentId)).find((node: FileNode) => node.name === name);
    if (!existing?.is_dir) throw e;
    return existing.id;
  }
}
