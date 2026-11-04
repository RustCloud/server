// The storage report for administrators: shared by the warning banner and the storage panel.

import { api, type StorageReport } from './api';

export const storage: { report: StorageReport | null } = $state({ report: null });

export async function loadStorage(): Promise<StorageReport | null> {
  try {
    storage.report = await api.storage();
  } catch {
    // Not an admin any more, or the server is unreachable: no banner.
  }
  return storage.report;
}

/** A disk percentage as text, e.g. "83". */
export function percentText(percent: number): string {
  return Math.round(percent).toString();
}
