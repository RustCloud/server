// The logged-in user, shared by all pages.

import { api, setUnauthorizedHandler, type User } from './api';

export const session: { user: User | null; loaded: boolean } = $state({ user: null, loaded: false });

/** Asks the server who we are (the session cookie is sent automatically). */
export async function loadSession() {
  try {
    session.user = await api.me();
  } catch {
    session.user = null;
  }
  session.loaded = true;
}

export async function logout() {
  try {
    await api.logout();
  } finally {
    session.user = null;
  }
}

// A request answered 401: the session expired or was revoked.
setUnauthorizedHandler(() => {
  session.user = null;
});
