import { describe, expect, it } from 'vitest';
import type { FileNode } from './api';
import { previewKind } from './preview';

function file(name: string, mime_type: string | null, is_dir = false): FileNode {
  return { id: '1', parent_id: null, name, is_dir, size: 1, mime_type, etag: null, created_at: '', updated_at: '', modified_at: '' };
}

describe('previewKind', () => {
  it('recognises media by type', () => {
    expect(previewKind(file('a.png', 'image/png'))).toBe('image');
    expect(previewKind(file('a.mp4', 'video/mp4'))).toBe('video');
    expect(previewKind(file('a.mp3', 'audio/mpeg'))).toBe('audio');
  });

  it('never previews SVG images, which can carry scripts', () => {
    expect(previewKind(file('logo.svg', 'image/svg+xml'))).toBeNull();
  });

  it('treats text types and known source extensions as text', () => {
    expect(previewKind(file('notes.txt', 'text/plain'))).toBe('text');
    expect(previewKind(file('data.json', 'application/json'))).toBe('text');
    expect(previewKind(file('main.rs', 'application/octet-stream'))).toBe('text');
    expect(previewKind(file('Dockerfile', null))).toBe('text');
  });

  it('downloads everything else', () => {
    expect(previewKind(file('archive.zip', 'application/zip'))).toBeNull();
    expect(previewKind(file('docs', null, true))).toBeNull();
  });
});
