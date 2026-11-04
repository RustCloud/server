import { describe, expect, it } from 'vitest';
import { allowedHeaders, appRequestUrl, BODY_LIMIT, requestBody } from './bridge';

const origin = 'https://cloud.example.com';

describe('appRequestUrl', () => {
  it('keeps requests under the app’s own routes', () => {
    expect(appRequestUrl('notes', '/list?tag=a', origin)).toBe('/app/notes/list?tag=a');
    expect(appRequestUrl('notes', '/', origin)).toBe('/app/notes/');
    expect(appRequestUrl('notes', '//evil.example.com/x', origin), 'a path of the app').toBe(
      '/app/notes//evil.example.com/x',
    );
  });

  it('refuses everything that leaves them', () => {
    for (const path of [
      '/../../api/v1/files/root',
      '/%2e%2e/%2e%2e/api/v1/auth/me',
      '/..\\..\\api',
      'api/v1',
      '/../calendar/',
      42,
    ]) {
      expect(appRequestUrl('notes', path, origin), String(path)).toBeNull();
    }
  });
});

describe('allowedHeaders', () => {
  it('passes only harmless headers', () => {
    expect(allowedHeaders({ 'Content-Type': 'text/plain', Authorization: 'Bearer x', Cookie: 'a=b' })).toEqual({
      'Content-Type': 'text/plain',
    });
    expect(allowedHeaders(null)).toEqual({});
  });
});

describe('requestBody', () => {
  it('takes text and bytes up to 16 MiB', () => {
    expect(requestBody(undefined)).toEqual({ body: null });
    expect(requestBody('{"a":1}')).toEqual({ body: '{"a":1}' });
    const bytes = new Uint8Array([0, 255, 1]).buffer;
    expect(requestBody(bytes)).toEqual({ body: bytes });
    expect(requestBody(new ArrayBuffer(BODY_LIMIT + 1))).toHaveProperty('error');
    expect(requestBody('x'.repeat(BODY_LIMIT + 1))).toHaveProperty('error');
    expect(requestBody(new ArrayBuffer(BODY_LIMIT + 1), 4 * BODY_LIMIT)).toEqual({ body: expect.any(ArrayBuffer) });
    expect(requestBody('12345', 4)).toHaveProperty('error', 'The body is larger than 0 MiB, what the app takes');
  });

  it('refuses anything else', () => {
    expect(requestBody({ a: 1 })).toHaveProperty('error');
    expect(requestBody(42)).toHaveProperty('error');
  });
});

describe('allowedHeaders for bytes', () => {
  it('passes ranges and versions, which media and edits need', () => {
    expect(allowedHeaders({ Range: 'bytes=0-99', 'If-Match': '"v1"', Origin: 'x' })).toEqual({
      Range: 'bytes=0-99',
      'If-Match': '"v1"',
    });
  });
});
