// Parsing of hash URLs: `#/files/<id>?q=x` → { parts: ['files', '<id>'], query }.

export interface Route {
  parts: string[];
  query: URLSearchParams;
}

export function parseHash(hash: string): Route {
  const [path, query = ''] = hash.replace(/^#/, '').split('?', 2);
  const parts = path
    .split('/')
    .filter(Boolean)
    .map((part) => decodeURIComponent(part));
  return { parts, query: new URLSearchParams(query) };
}
