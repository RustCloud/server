<!--
  Administration → Plugins → Catalog: plugins from the internet with a
  search, filters (all, installed, updates) and one button each — Install,
  Update to the newest version that works here, or a grey Update when the
  newest is installed. Installing works like an archive uploaded by hand: a
  new plugin starts disabled.
-->
<script lang="ts">
  import { api, type CatalogItem, type CatalogView, type Plugin } from '../lib/api';
  import { formatBytes } from '../lib/format';
  import { errorText, locale, t } from '../lib/i18n';
  import Dialog from './Dialog.svelte';
  import Icon from './Icon.svelte';

  let { onchanged, onclose }: { onchanged: (plugin: Plugin) => void; onclose: () => void } = $props();

  type Filter = 'all' | 'installed' | 'updates';

  let view = $state<CatalogView | null>(null);
  let loading = $state(true);
  let error = $state('');
  let message = $state('');
  let query = $state('');
  let filter = $state<Filter>('all');
  /** The plugin being installed; one at a time. */
  let busy = $state<string | null>(null);

  async function load(fresh = false) {
    loading = true;
    try {
      view = await api.pluginCatalog(fresh);
      error = '';
    } catch (e) {
      error = errorText(e);
    } finally {
      loading = false;
    }
  }

  const text = (texts: Record<string, string> | undefined) => (texts ? (texts[locale] ?? texts.en ?? '') : '');
  const installed = (item: CatalogItem) => item.installed !== null || item.action === 'built_in';

  const updates = $derived(view?.plugins.filter((p) => p.action === 'update').length ?? 0);
  const shown = $derived.by(() => {
    const words = query.toLowerCase().split(/\s+/).filter(Boolean);
    return (view?.plugins ?? []).filter((item) => {
      if (filter === 'installed' && !installed(item)) return false;
      if (filter === 'updates' && item.action !== 'update') return false;
      const haystack = [
        item.id,
        item.author,
        ...Object.values(item.name),
        ...Object.values(item.description),
      ]
        .join(' ')
        .toLowerCase();
      return words.every((word) => haystack.includes(word));
    });
  });
  const host = $derived.by(() => {
    try {
      return view?.url ? new URL(view.url).host : '';
    } catch {
      return '';
    }
  });

  async function install(item: CatalogItem) {
    if (!item.release || busy) return;
    busy = item.id;
    message = '';
    error = '';
    const update = item.action === 'update';
    try {
      const plugin = await api.installFromCatalog(item.id, item.release.version);
      message = update
        ? t.catalogUpdatedNote(text(item.name), item.release.version)
        : t.catalogInstalledNote(text(item.name));
      onchanged(plugin);
      view = await api.pluginCatalog();
    } catch (e) {
      error = errorText(e);
    } finally {
      busy = null;
    }
  }

  /** A soft colour of its own for each plugin's initial. */
  function hue(id: string): number {
    let hash = 0;
    for (const char of id) hash = (hash * 31 + char.charCodeAt(0)) % 360;
    return hash;
  }

  load();
</script>

<Dialog title={t.catalogTitle} {onclose} large>
  <div class="toolbar">
    <label class="search">
      <Icon name="search" size={16} />
      <input type="search" placeholder={t.catalogSearch} aria-label={t.catalogSearch} bind:value={query} />
    </label>
    <div class="filters" role="group">
      {#each ['all', 'installed', 'updates'] as const as name (name)}
        <button type="button" class:on={filter === name} aria-pressed={filter === name} onclick={() => (filter = name)}>
          {t.catalogFilter[name]}
          {#if name === 'updates' && updates > 0}<span class="count">{updates}</span>{/if}
        </button>
      {/each}
    </div>
    <button
      type="button"
      class="refresh"
      title={t.catalogRefresh}
      aria-label={t.catalogRefresh}
      disabled={loading}
      onclick={() => load(true)}
    >
      <Icon name="refresh" size={16} />
    </button>
  </div>

  {#if host || view?.skipped.length}
    <p class="muted small source">
      {#if host}{t.catalogFrom(host)}{/if}
      {#if view?.skipped.length}<span title={view.skipped.join('\n')}>{t.catalogSkipped(view.skipped.length)}</span>{/if}
    </p>
  {/if}
  {#if message}<p class="note small" role="status">{message}</p>{/if}
  {#if error}<p class="error small" role="alert">{error}</p>{/if}

  <div class="list" aria-busy={loading}>
    {#if loading && !view}
      {#each [1, 2, 3] as n (n)}<div class="item skeleton"></div>{/each}
    {:else if view && view.plugins.length === 0}
      <p class="empty muted">{t.catalogEmpty}</p>
    {:else if view && shown.length === 0}
      <p class="empty muted">{t.catalogNothingFound}</p>
    {:else}
      {#each shown as item (item.id)}
        {@const notes = text(item.release?.notes)}
        <article class="item">
          <div class="avatar" style="--hue: {hue(item.id)}" aria-hidden="true">
            {text(item.name).charAt(0).toUpperCase()}
          </div>
          <div class="body">
            <div class="name">
              <strong>{text(item.name)}</strong>
              {#if item.release}<span class="badge">{item.release.version}</span>{/if}
              {#if item.installed}
                <span class="badge installed">{t.catalogInstalledVersion(item.installed)}</span>
              {/if}
            </div>
            <p class="description">{text(item.description)}</p>
            <p class="meta muted small">
              {item.author} · {item.license}
              {#if item.release}· {formatBytes(item.release.size)}{/if}
              · <a href={item.source} target="_blank" rel="noopener noreferrer">{t.catalogSourceCode}</a>
              {#if item.homepage}
                · <a href={item.homepage} target="_blank" rel="noopener noreferrer">{t.catalogHomepage}</a>
              {/if}
            </p>
            {#if notes && item.action === 'update'}
              <p class="notes small"><strong>{t.catalogWhatsNew}:</strong> {notes}</p>
            {/if}
          </div>
          <div class="action">
            {#if busy === item.id}
              <button type="button" class="primary" disabled><span class="spinner"></span>{t.catalogInstalling}</button>
            {:else if item.action === 'install'}
              <button type="button" class="primary" disabled={busy !== null} onclick={() => install(item)}>
                <Icon name="download" size={16} />{t.catalogInstall}
              </button>
            {:else if item.action === 'update' && item.release}
              <button type="button" class="primary" disabled={busy !== null} onclick={() => install(item)}>
                <Icon name="up" size={16} />{t.catalogUpdateTo(item.release.version)}
              </button>
            {:else if item.action === 'latest'}
              <button type="button" disabled title={t.catalogUpToDate}>{t.catalogUpdate}</button>
            {:else if item.action === 'needs_newer_server'}
              <button
                type="button"
                disabled
                title={t.catalogNeedsServerHint(item.versions.map((v) => v.api).join(', '))}
              >
                {t.catalogNeedsServer}
              </button>
            {:else}
              <button type="button" disabled>{t.catalogBuiltIn}</button>
            {/if}
          </div>
        </article>
      {/each}
    {/if}
  </div>
</Dialog>

<style>
  .toolbar {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
  }
  .search {
    flex: 1 1 220px;
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 0 10px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--surface);
    color: var(--muted);
  }
  .search:focus-within {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  .search input {
    flex: 1;
    min-width: 0;
    border: 0;
    padding: 8px 0;
    background: transparent;
  }
  .search input:focus-visible {
    outline: none;
  }
  .filters {
    display: inline-flex;
    border: 1px solid var(--border);
    border-radius: 8px;
    overflow: hidden;
  }
  .filters button {
    border: 0;
    border-radius: 0;
    padding: 7px 12px;
  }
  .filters button + button {
    border-left: 1px solid var(--border);
  }
  .filters button.on {
    background: var(--selected);
    color: var(--accent);
  }
  .count {
    min-width: 18px;
    padding: 0 5px;
    border-radius: 9px;
    background: var(--accent);
    color: #fff;
    font-size: 0.75rem;
    line-height: 18px;
    text-align: center;
  }
  .refresh {
    padding: 8px;
  }
  p {
    margin: 0;
  }
  .source {
    display: flex;
    gap: 12px;
    margin-top: 8px;
  }
  .note {
    margin-top: 10px;
    padding: 8px 12px;
    border-radius: 8px;
    background: var(--selected);
  }
  .error {
    margin-top: 10px;
  }
  .list {
    display: grid;
    gap: 10px;
    margin-top: 14px;
    max-height: min(60vh, 640px);
    overflow-y: auto;
    padding-right: 2px;
  }
  .item {
    display: grid;
    grid-template-columns: 44px 1fr auto;
    gap: 14px;
    align-items: start;
    padding: 14px;
    border: 1px solid var(--border);
    border-radius: 10px;
    background: var(--surface);
    transition: border-color 0.15s;
  }
  .item:hover {
    border-color: color-mix(in srgb, var(--accent) 40%, var(--border));
  }
  .skeleton {
    height: 96px;
    background: linear-gradient(90deg, var(--hover), var(--border-soft), var(--hover));
    background-size: 200% 100%;
    animation: shimmer 1.2s infinite linear;
  }
  @keyframes shimmer {
    to {
      background-position: -200% 0;
    }
  }
  .avatar {
    width: 44px;
    height: 44px;
    border-radius: 10px;
    display: grid;
    place-items: center;
    font-weight: 600;
    font-size: 1.2rem;
    background: hsl(var(--hue) 70% 92%);
    color: hsl(var(--hue) 55% 32%);
  }
  @media (prefers-color-scheme: dark) {
    .avatar {
      background: hsl(var(--hue) 35% 22%);
      color: hsl(var(--hue) 70% 78%);
    }
  }
  .body {
    min-width: 0;
    display: grid;
    gap: 4px;
  }
  .name {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
  }
  .badge {
    font-size: 0.75rem;
    padding: 1px 7px;
    border-radius: 999px;
    border: 1px solid var(--border);
    color: var(--muted);
  }
  .badge.installed {
    border-color: transparent;
    background: color-mix(in srgb, var(--ok) 14%, transparent);
    color: var(--ok);
  }
  .description {
    overflow-wrap: anywhere;
  }
  .meta a {
    text-decoration: none;
  }
  .meta a:hover {
    text-decoration: underline;
  }
  .notes {
    padding: 6px 10px;
    border-left: 3px solid var(--accent);
    background: var(--hover);
    border-radius: 0 6px 6px 0;
  }
  .action button {
    white-space: nowrap;
  }
  .empty {
    padding: 32px 0;
    text-align: center;
  }
  .spinner {
    width: 14px;
    height: 14px;
    border: 2px solid currentColor;
    border-right-color: transparent;
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  @media (max-width: 560px) {
    .item {
      grid-template-columns: 36px 1fr;
    }
    .avatar {
      width: 36px;
      height: 36px;
      font-size: 1rem;
    }
    .action {
      grid-column: 1 / -1;
    }
    .action button {
      width: 100%;
      justify-content: center;
    }
  }
</style>
