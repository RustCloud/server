<!-- Top bar: brand, file search and the user menu (personal settings, logout). -->
<script lang="ts">
  import type { User } from '../lib/api';
  import { t } from '../lib/i18n';
  import { go, href } from '../lib/router.svelte';
  import { logout } from '../lib/session.svelte';
  import Icon from './Icon.svelte';
  import Logo from './Logo.svelte';

  let { user }: { user: User } = $props();

  let query = $state('');

  function search(event: SubmitEvent) {
    event.preventDefault();
    if (query.trim()) go('/search?q=' + encodeURIComponent(query.trim()));
  }

  function closeMenu(event: Event) {
    (event.currentTarget as HTMLElement).closest('details')?.removeAttribute('open');
  }
</script>

<header class="topbar">
  <a class="brand" href={href('/files')}><Logo size={26} /> <span>{t.appName}</span></a>

  <form class="search" onsubmit={search} role="search">
    <Icon name="search" />
    <input type="search" placeholder={t.searchPlaceholder} bind:value={query} />
  </form>

  <details class="menu user-menu">
    <summary title={user.username}>
      <Icon name="user" />
      <span class="name">{user.display_name}</span>
      <Icon name="chevron" size={14} />
    </summary>
    <div class="menu-items">
      <a href={href('/settings')} onclick={closeMenu}><Icon name="gear" /> {t.settings}</a>
      <button type="button" onclick={logout}><Icon name="logout" /> {t.logout}</button>
    </div>
  </details>
</header>

<style>
  .topbar {
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 8px 16px;
    border-bottom: 1px solid var(--border);
    background: var(--surface);
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 8px;
    font-weight: 600;
    font-size: 1.1rem;
    color: var(--accent);
    text-decoration: none;
  }
  .search {
    flex: 1;
    min-width: 0;
    max-width: 420px;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 0 10px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--bg);
    color: var(--muted);
  }
  .search input {
    border: none;
    background: transparent;
    padding: 8px 0;
    flex: 1;
    min-width: 0;
    outline: none;
  }
  .user-menu {
    margin-left: auto;
  }
  .user-menu summary {
    gap: 6px;
    align-items: center;
    padding: 6px 10px;
  }
  @media (max-width: 640px) {
    .brand span,
    .user-menu .name {
      display: none;
    }
  }
</style>
