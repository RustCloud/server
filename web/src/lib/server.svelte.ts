// Facts about the server the UI needs (loaded once, after login).

import { api } from './api';

export const server: { publicUrl: string } = $state({ publicUrl: location.origin });

export async function loadServerInfo() {
  try {
    const info = await api.info();
    if (info.public_url) server.publicUrl = info.public_url;
  } catch {
    // Keep the current address; links still work from here.
  }
}
