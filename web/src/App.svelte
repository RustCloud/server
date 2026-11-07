<!-- The app shell: top bar, the apps panel and the page for the current URL. Public links work without logging in. -->
<script lang="ts">
  import { locale, t } from './lib/i18n';
  import { route } from './lib/router.svelte';
  import { href } from './lib/router.svelte';
  import { loadApps } from './lib/apps.svelte';
  import { loadServerInfo } from './lib/server.svelte';
  import { loadStorage, percentText, storage } from './lib/storage.svelte';
  import { loadSession, session } from './lib/session.svelte';
  import AppFrame from './components/AppFrame.svelte';
  import { apps } from './lib/apps.svelte';
  import Sidebar from './components/Sidebar.svelte';
  import TopBar from './components/TopBar.svelte';
  import Admin from './pages/Admin.svelte';
  import Connect from './pages/Connect.svelte';
  import Files from './pages/Files.svelte';
  import Links from './pages/Links.svelte';
  import Login from './pages/Login.svelte';
  import Public from './pages/Public.svelte';
  import Search from './pages/Search.svelte';
  import Settings from './pages/Settings.svelte';

  const page = $derived(route.parts[0] ?? 'files');
  const isPublic = $derived(page === 's' && !!route.parts[1]);

  /** An app's name, also for apps that are not in the side panel. */
  function appTitle(id: string): string {
    const name = apps.manifests.find((a) => a.id === id)?.name;
    return name?.[locale] ?? name?.en ?? id;
  }

  loadSession();

  $effect(() => {
    if (!session.user) return;
    loadServerInfo();
    loadApps();
  });

  // Administrators get a warning when the storage disk is nearly full.
  $effect(() => {
    if (!session.user?.is_admin) return;
    loadStorage();
    const timer = setInterval(loadStorage, 10 * 60 * 1000);
    return () => clearInterval(timer);
  });

  /** Keeps at most one menu open: a click elsewhere (or on another menu) closes the rest. */
  function closeOtherMenus(event: MouseEvent) {
    for (const menu of document.querySelectorAll('details.menu[open]')) {
      if (!menu.contains(event.target as Node)) menu.removeAttribute('open');
    }
  }

  function closeMenusOnEscape(event: KeyboardEvent) {
    if (event.key !== 'Escape') return;
    for (const menu of document.querySelectorAll('details.menu[open]')) menu.removeAttribute('open');
  }
</script>

<svelte:window onclick={closeOtherMenus} onkeydown={closeMenusOnEscape} />

{#if isPublic}
  <Public token={route.parts[1]} nodeId={route.parts[2]} />
{:else if !session.loaded}
  <p class="muted loading">{t.loading}</p>
{:else if !session.user}
  <Login />
{:else}
  <div class="shell">
    <TopBar user={session.user} />
    <div class="body">
      <Sidebar isAdmin={session.user.is_admin} />
      <main class="page" class:app-page={page === 'app'}>
        {#if session.user.is_admin && storage.report?.nearly_full && storage.report.used_percent !== null}
          <a class="warning banner" href={href('/admin/system')}>{t.nearlyFull(percentText(storage.report.used_percent))}</a>
        {/if}
        {#if page === 'search'}
          <Search query={route.query.get('q') ?? ''} />
        {:else if page === 'links'}
          <Links />
        {:else if page === 'settings'}
          <Settings />
        {:else if page === 'connect'}
          <Connect />
        {:else if page === 'app' && route.parts[1]}
          <AppFrame
            id={route.parts[1]}
            path={route.parts.slice(2).join('/')}
            query={route.query.toString()}
            title={appTitle(route.parts[1])}
          />
        {:else if page === 'admin' && session.user.is_admin}
          <Admin tab={route.parts[1]} />
        {:else}
          <Files folderId={page === 'files' ? route.parts[1] : undefined} trash={page === 'trash'} />
        {/if}
      </main>
    </div>
  </div>
{/if}

<style>
  .shell {
    display: flex;
    flex-direction: column;
    height: 100vh;
  }
  .body {
    flex: 1;
    display: flex;
    min-height: 0;
  }
  .page {
    flex: 1;
    min-width: 0;
    overflow-y: auto;
    padding: 20px 24px 80px;
  }
  /* An app's page fills what the banner (if any) leaves. */
  .app-page {
    display: flex;
    flex-direction: column;
    padding: 0;
    overflow: hidden;
  }
  .app-page .banner {
    margin: 12px 16px;
  }
  .banner {
    display: block;
    margin-bottom: 16px;
    text-decoration: none;
  }
  .loading {
    text-align: center;
    margin-top: 30vh;
  }
  @media (max-width: 640px) {
    .page {
      padding: 16px 12px 80px;
    }
  }
</style>
