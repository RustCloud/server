// A tiny hash router: `#/files/<id>` → { parts: ['files', '<id>'], query }.
//
// Hash URLs need no server configuration and never collide with API paths.

import { parseHash, type Route } from './hash';

export type { Route };

/** The current route; components read it reactively. */
export const route: Route = $state(parseHash(location.hash));

window.addEventListener('hashchange', () => {
  Object.assign(route, parseHash(location.hash));
});

/** Navigates to an app path such as `/files/123`. */
export function go(path: string) {
  location.hash = path;
}

/** Builds an `href` for an app path. */
export function href(path: string): string {
  return '#' + path;
}
