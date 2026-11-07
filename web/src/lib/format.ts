// Human-friendly sizes and dates.

const UNITS = ['B', 'KB', 'MB', 'GB', 'TB'];

/** `1536` → `"1.5 KB"`. Uses 1024-based units, like most file managers. */
export function formatBytes(bytes: number): string {
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < UNITS.length - 1) {
    value /= 1024;
    unit += 1;
  }
  const digits = unit === 0 || value >= 10 ? 0 : 1;
  return `${value.toFixed(digits)} ${UNITS[unit]}`;
}

/** Date and time in the browser's locale, e.g. `27 Oct 2026, 21:05`. */
export function formatDate(iso: string, locale?: string): string {
  return new Intl.DateTimeFormat(locale, { dateStyle: 'medium', timeStyle: 'short' }).format(new Date(iso));
}

/** Name for a copy placed next to the original: `report.pdf` → `report (copy).pdf`. */
export function copyName(name: string, isDir: boolean): string {
  const dot = name.lastIndexOf('.');
  if (isDir || dot <= 0) return `${name} (copy)`;
  return `${name.slice(0, dot)} (copy)${name.slice(dot)}`;
}

/** A short name for a browser from its User-Agent: `Firefox, Linux`. */
export function describeAgent(agent: string): string {
  const browsers: [RegExp, string][] = [
    [/Edg\//, 'Edge'],
    [/OPR\/|Opera/, 'Opera'],
    [/YaBrowser\//, 'Yandex Browser'],
    [/Firefox\//, 'Firefox'],
    [/Chrome\//, 'Chrome'],
    [/Safari\//, 'Safari'],
  ];
  const systems: [RegExp, string][] = [
    [/Android/, 'Android'],
    [/iPhone|iPad/, 'iOS'],
    [/Windows/, 'Windows'],
    [/Mac OS X|Macintosh/, 'macOS'],
    [/CrOS/, 'ChromeOS'],
    [/Linux/, 'Linux'],
  ];
  const browser = browsers.find(([pattern]) => pattern.test(agent))?.[1];
  const system = systems.find(([pattern]) => pattern.test(agent))?.[1];
  if (!browser && !system) return agent.slice(0, 60);
  return [browser, system].filter(Boolean).join(', ');
}
