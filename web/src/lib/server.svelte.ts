// Facts about the server the UI needs (loaded after login, and on public link pages).

import { api } from './api';

export const server: { publicUrl: string; thumbnails: boolean } = $state({
  publicUrl: location.origin,
  thumbnails: false,
});

export async function loadServerInfo() {
  try {
    const info = await api.info();
    if (info.public_url) server.publicUrl = info.public_url;
    server.thumbnails = info.thumbnails;
  } catch {
    // Keep the current address; links still work from here.
  }
}
