// The bridge between the panel and an app page in its sandboxed <iframe>.
//
// The page runs in a unique origin, so it can't send the user's cookie itself.
// It asks the panel with postMessage (see public/app-sdk.js); the panel makes
// the request for it — only to the app's own routes, never to the API.

/** A request an app page asks the panel to make. */
export interface BridgeRequest {
  rustcloud: 'fetch';
  id: number;
  method?: string;
  /** Path below `/app/<id>`, starting with `/`. */
  path: string;
  headers?: Record<string, string>;
  /** Text, or bytes (sent transferred, not copied). */
  body?: string | ArrayBuffer | null;
}

/** The largest body a page sends when the server doesn't say: what an app takes at ×1. */
export const BODY_LIMIT = 16 * 1024 * 1024;

/**
 * The body to send for a page's request: text or bytes, `null` for none, or
 * an error message if it can't be sent. `limit`: the most the app takes at
 * once (`GET /api/v1/apps`, `limits`).
 */
export function requestBody(
  body: unknown,
  limit = BODY_LIMIT,
): { body: string | ArrayBuffer | null } | { error: string } {
  const tooLarge = { error: `The body is larger than ${Math.floor(limit / 1024 / 1024)} MiB, what the app takes` };
  if (body === undefined || body === null) return { body: null };
  if (typeof body === 'string') return new Blob([body]).size > limit ? tooLarge : { body };
  if (body instanceof ArrayBuffer) return body.byteLength > limit ? tooLarge : { body };
  return { error: 'The body must be text or bytes' };
}

/**
 * The same-origin URL (path and query) for `path` of app `appId`, or null if
 * it would leave the app's routes (e.g. `/../../api/v1/...`).
 */
export function appRequestUrl(appId: string, path: unknown, origin: string): string | null {
  if (typeof path !== 'string' || !path.startsWith('/')) return null;
  const base = `/app/${encodeURIComponent(appId)}`;
  let url: URL;
  try {
    url = new URL(base + path, origin);
  } catch {
    return null;
  }
  if (url.origin !== origin) return null;
  if (url.pathname !== base && !url.pathname.startsWith(base + '/')) return null;
  return url.pathname + url.search;
}

/** Headers an app page may set on its requests; the panel adds the rest. */
const ALLOWED_HEADERS = ['content-type', 'accept', 'range', 'if-match', 'if-none-match'];

/** Headers of the app's answer the page gets. */
export const ANSWER_HEADERS = ['content-type', 'content-disposition', 'content-range', 'etag', 'last-modified'];

export function allowedHeaders(headers: unknown): Record<string, string> {
  if (!headers || typeof headers !== 'object') return {};
  const result: Record<string, string> = {};
  for (const [name, value] of Object.entries(headers)) {
    if (ALLOWED_HEADERS.includes(name.toLowerCase()) && typeof value === 'string') result[name] = value;
  }
  return result;
}
