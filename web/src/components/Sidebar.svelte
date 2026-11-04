<!-- Left panel: the list of apps, and administration for admins. Collapses to icons. -->
<script lang="ts">
  import { isActive } from '../lib/apps';
  import { apps } from '../lib/apps.svelte';
  import { t } from '../lib/i18n';
  import { href, route } from '../lib/router.svelte';
  import Icon, { iconOrDefault } from './Icon.svelte';

  let { isAdmin }: { isAdmin: boolean } = $props();

  const STORAGE_KEY = 'rustcloud.sidebar.collapsed';
  let collapsed = $state(readCollapsed());

  /** The saved choice; narrow screens start collapsed. */
  function readCollapsed(): boolean {
    try {
      const saved = localStorage.getItem(STORAGE_KEY);
      if (saved !== null) return saved === '1';
    } catch {
      // Storage may be blocked; fall through to the default.
    }
    return window.matchMedia('(max-width: 700px)').matches;
  }

  function toggle() {
    collapsed = !collapsed;
    try {
      localStorage.setItem(STORAGE_KEY, collapsed ? '1' : '0');
    } catch {
      // Not remembered, but still works.
    }
  }

  const current = $derived(route.parts[0] ?? 'files');
</script>

<nav class="sidebar" class:collapsed aria-label={t.apps}>
  {#each apps.entries as app (app.id)}
    <a href={href(app.href)} class:active={isActive(app, route.parts.length ? route.parts : ['files'])} title={collapsed ? app.title : undefined}>
      <Icon name={iconOrDefault(app.icon)} size={20} />
      <span class="label">{app.title}</span>
    </a>
  {/each}

  <div class="bottom">
    {#if isAdmin}
      <a href={href('/admin')} class:active={current === 'admin'} title={collapsed ? t.administration : undefined}>
        <Icon name="gear" size={20} />
        <span class="label">{t.administration}</span>
      </a>
    {/if}
    <button type="button" class="toggle" onclick={toggle} title={collapsed ? t.expand : t.collapse}>
      <Icon name={collapsed ? 'expand' : 'collapse'} size={20} />
      <span class="label">{t.collapse}</span>
    </button>
  </div>
</nav>

<style>
  .sidebar {
    display: flex;
    flex-direction: column;
    gap: 2px;
    width: 200px;
    padding: 12px 8px;
    border-right: 1px solid var(--border);
    background: var(--surface);
    position: sticky;
    top: 0;
    height: 100%;
    transition: width 0.15s;
  }
  .sidebar.collapsed {
    width: 56px;
  }
  a,
  .toggle {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 8px 10px;
    border-radius: 8px;
    color: var(--text);
    text-decoration: none;
    white-space: nowrap;
    overflow: hidden;
  }
  .toggle {
    border: none;
    background: none;
    width: 100%;
    color: var(--muted);
  }
  a:hover,
  .toggle:hover {
    background: var(--hover);
  }
  a.active {
    background: var(--selected);
    color: var(--accent);
  }
  .collapsed .label {
    display: none;
  }
  .bottom {
    margin-top: auto;
    display: grid;
    gap: 2px;
  }
</style>
