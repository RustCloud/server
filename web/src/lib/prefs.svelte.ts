// Display preferences of this browser, kept in localStorage.

const SHOW_HIDDEN = 'rustcloud.showHidden';

export const prefs = $state({ showHidden: readFlag(SHOW_HIDDEN) });

export function setShowHidden(value: boolean) {
  prefs.showHidden = value;
  try {
    localStorage.setItem(SHOW_HIDDEN, value ? '1' : '0');
  } catch {
    // Not remembered, but works until the page is reloaded.
  }
}

/** Hidden files and folders start with a dot, e.g. `.appdata` (as in Unix). */
export function isHidden(node: { name: string }): boolean {
  return node.name.startsWith('.');
}

/** Whether a node is listed with the current preferences. */
export function isShown(node: { name: string }): boolean {
  return prefs.showHidden || !isHidden(node);
}

function readFlag(key: string): boolean {
  try {
    return localStorage.getItem(key) === '1';
  } catch {
    return false;
  }
}
