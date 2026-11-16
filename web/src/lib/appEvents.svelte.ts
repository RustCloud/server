// What the panel hears of the user's plugins over one connection
// (`/api/v1/apps/events`): the numbers by their entries in the side panel,
// rings over any page, and "someone sent you something" for an open page.

import { api } from './api';

export interface Ring {
  id: string;
  app: string;
  /** By username, which the sender can't change. */
  from: { id: string; username: string };
  kind: 'call' | 'message' | 'invite';
  ref: string;
  /** When it ends by itself, RFC 3339. */
  until: string;
}

export const appEvents: { badges: Record<string, number>; rings: Ring[] } = $state({ badges: {}, rings: [] });

const changedListeners = new Set<(app: string) => void>();

/** Calls `listener(app)` when a job someone sent the user is done in `app`; returns the way to stop. */
export function onChanged(listener: (app: string) => void): () => void {
  changedListeners.add(listener);
  return () => changedListeners.delete(listener);
}

/** Listens while the user is logged in; returns the way to stop. The server ends a connection after a while. */
export function connectAppEvents(): () => void {
  let source: EventSource | null = null;
  let retry: ReturnType<typeof setTimeout> | undefined;
  let stopped = false;

  const data = (event: Event) => JSON.parse((event as MessageEvent).data);

  function open() {
    source = new EventSource('/api/v1/apps/events');
    // A connection starts with what is current: badges and rings on.
    source.onopen = () => {
      appEvents.badges = {};
      appEvents.rings = [];
    };
    source.addEventListener('badge', (event) => {
      const { app, count } = data(event);
      const badges = { ...appEvents.badges };
      if (count > 0) badges[app] = count;
      else delete badges[app];
      appEvents.badges = badges;
    });
    source.addEventListener('ring', (event) => {
      const ring: Ring = data(event);
      appEvents.rings = [...appEvents.rings.filter((r) => r.id !== ring.id), ring];
    });
    source.addEventListener('ring-end', (event) => {
      const { id } = data(event);
      appEvents.rings = appEvents.rings.filter((r) => r.id !== id);
    });
    source.addEventListener('changed', (event) => {
      const { app } = data(event);
      for (const listener of changedListeners) listener(app);
    });
    source.onerror = () => {
      source?.close();
      if (!stopped) retry = setTimeout(open, 5000);
    };
  }

  open();
  return () => {
    stopped = true;
    clearTimeout(retry);
    source?.close();
  };
}

/**
 * Takes a ring away, when the user opened or declined it: in this tab now, in
 * the others (and on other devices) through the server.
 */
export async function endRing(ring: Ring) {
  appEvents.rings = appEvents.rings.filter((r) => r.id !== ring.id);
  try {
    await api.endRing(ring.id);
  } catch {
    // It ends by itself.
  }
}

/** The rings still on: one that outlived its time is gone even if its end got lost. */
export function forgetEndedRings() {
  const now = Date.now();
  const on = appEvents.rings.filter((ring) => Date.parse(ring.until) > now);
  if (on.length !== appEvents.rings.length) appEvents.rings = on;
}
