<script lang="ts" module>
  // Simple stroke icons on a 24×24 grid.
  const PATHS = {
    folder: 'M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z',
    file: 'M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8zM14 3v5h5',
    image: 'M4 5h16v14H4zM4 16l5-5 4 4 3-3 4 4M15 9h.01',
    upload: 'M12 16V4M7 9l5-5 5 5M5 20h14',
    download: 'M12 4v12M7 11l5 5 5-5M5 20h14',
    plus: 'M12 5v14M5 12h14',
    more: 'M12 6h.01M12 12h.01M12 18h.01',
    trash: 'M4 7h16M10 11v6M14 11v6M6 7l1 13h10l1-13M9 7V4h6v3',
    link: 'M10 14a4 4 0 0 0 5.66 0l3-3a4 4 0 0 0-5.66-5.66l-1 1M14 10a4 4 0 0 0-5.66 0l-3 3a4 4 0 0 0 5.66 5.66l1-1',
    search: 'M11 18a7 7 0 1 1 0-14 7 7 0 0 1 0 14zM20 20l-4-4',
    home: 'M3 11l9-7 9 7M5 10v10h14V10',
    close: 'M6 6l12 12M18 6L6 18',
    cloud: 'M7 18a5 5 0 0 1-.6-9.97A7 7 0 0 1 20 9a4.5 4.5 0 0 1 0 9z',
    up: 'M12 19V5M5 12l7-7 7 7',
    user: 'M12 12a4 4 0 1 0 0-8 4 4 0 0 0 0 8zM4 21a8 8 0 0 1 16 0',
    chevron: 'M6 9l6 6 6-6',
    logout: 'M15 4h3a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2h-3M10 17l-5-5 5-5M5 12h11',
    gear: 'M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6zM19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z',
    left: 'M15 6l-6 6 6 6',
    right: 'M9 6l6 6-6 6',
    collapse: 'M4 5h16v14H4zM9 5v14M15 10l-2 2 2 2',
    expand: 'M4 5h16v14H4zM9 5v14M13 10l2 2-2 2',
    app: 'M4 4h6v6H4zM14 4h6v6h-6zM4 14h6v6H4zM14 14h6v6h-6z',
    note: 'M6 3h9l3 3v15H6zM9 10h6M9 14h6M9 18h4',
    calendar: 'M4 6h16v14H4zM4 10h16M8 3v5M16 3v5',
    lock: 'M6 11h12v9H6zM8 11V8a4 4 0 0 1 8 0v3',
    refresh: 'M20 12a8 8 0 1 1-2.34-5.66M20 4v5h-5',
    history: 'M3 12a9 9 0 1 0 2.6-6.4L3 8M3 3v5h5M12 7v5l3 3',
  };
  export type IconName = keyof typeof PATHS;

  /** An icon named by the server (e.g. in an app manifest); a generic one if unknown. */
  export function iconOrDefault(name: string): IconName {
    return name in PATHS ? (name as IconName) : 'app';
  }
</script>

<script lang="ts">
  let { name, size = 18 }: { name: IconName; size?: number } = $props();
</script>

<svg
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width={name === 'more' ? 3 : 1.8}
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
>
  <path d={PATHS[name]} />
</svg>
