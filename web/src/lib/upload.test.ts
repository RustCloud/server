import { beforeEach, describe, expect, it, vi } from 'vitest';
import { ApiError, api, type FileNode } from './api';
import { entriesFromInput, FolderMaker } from './upload';

function folder(id: string, name: string): FileNode {
  return {
    id,
    parent_id: 'root',
    name,
    is_dir: true,
    size: 0,
    mime_type: null,
    etag: null,
    created_at: '',
    updated_at: '',
    modified_at: '',
  };
}

describe('FolderMaker', () => {
  beforeEach(() => vi.restoreAllMocks());

  it('creates each folder once, even for parallel requests', async () => {
    let next = 0;
    const create = vi
      .spyOn(api, 'createFolder')
      .mockImplementation(async (_parent, name) => folder(`id${next++}`, name));
    const maker = new FolderMaker('root');

    const [b, c] = await Promise.all([maker.folderId(['a', 'b']), maker.folderId(['a', 'c'])]);
    expect(create.mock.calls.map(([parent, name]) => `${parent}/${name}`)).toEqual(['root/a', 'id0/b', 'id0/c']);
    expect([b, c]).toEqual(['id1', 'id2']);
    expect(await maker.folderId([])).toBe('root');
  });

  it('reuses a folder that already exists', async () => {
    vi.spyOn(api, 'createFolder').mockRejectedValue(new ApiError(409, 'NAME_TAKEN', 'exists'));
    vi.spyOn(api, 'children').mockResolvedValue([folder('old', 'photos')]);
    expect(await new FolderMaker('root').folderId(['photos'])).toBe('old');
  });
});

describe('entriesFromInput', () => {
  it('splits the relative path of folder uploads', () => {
    const file = new File(['x'], 'a.txt');
    Object.defineProperty(file, 'webkitRelativePath', { value: 'trip/day1/a.txt' });
    const list = { 0: file, length: 1, item: () => file, [Symbol.iterator]: [file][Symbol.iterator].bind([file]) };
    expect(entriesFromInput(list as unknown as FileList)).toEqual([{ folders: ['trip', 'day1'], file }]);
  });
});
