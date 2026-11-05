<!--
  Administration → Plugins: every plugin of the server with its state,
  install / remove, enable / disable; "Configure" opens the plugin in a
  dialog with its settings, its status and what the server gives it.
-->
<script lang="ts">
  import { api, type Plugin } from '../lib/api';
  import { loadApps } from '../lib/apps.svelte';
  import { errorText, locale, t } from '../lib/i18n';
  import PluginCatalog from './PluginCatalog.svelte';
  import PluginDialog from './PluginDialog.svelte';

  let plugins = $state<Plugin[]>([]);
  /** The most memory one plugin may get, as a multiple (`[plugins] max_memory`). */
  let maxMemory = $state(16);
  let error = $state('');
  let message = $state('');
  let installing = $state(false);
  let catalogOpen = $state(false);
  let archiveInput = $state<HTMLInputElement>();
  /** The plugin open in the dialog. */
  let openId = $state<string | null>(null);
  const opened = $derived(plugins.find((p) => p.id === openId));
  /** What the background jobs of all running plugins may take at once. */
  const jobsMemory = $derived(
    plugins.filter((p) => p.status.state === 'running').reduce((sum, p) => sum + p.effective.jobs_mib, 0),
  );

  async function load() {
    try {
      const [list, sections] = await Promise.all([api.plugins(), api.settings()]);
      plugins = list;
      const section = sections.find((s) => s.name === 'plugins');
      if (typeof section?.values.max_memory === 'number') maxMemory = section.values.max_memory;
      error = '';
    } catch (e) {
      error = errorText(e);
    }
  }

  const name = (plugin: Plugin) =>
    plugin.manifest ? (plugin.manifest.name[locale] ?? plugin.manifest.name.en ?? plugin.id) : plugin.id;

  async function setEnabled(plugin: Plugin, enabled: boolean) {
    try {
      replace(await api.configurePlugin(plugin.id, { enabled, limits: plugin.limits }));
      error = '';
      await loadApps();
    } catch (e) {
      error = errorText(e);
    }
  }

  async function install(file: File | undefined) {
    if (!file) return;
    installing = true;
    message = '';
    try {
      const installed = await api.installPlugin(file);
      message = t.pluginInstalled(name(installed), installed.manifest?.version ?? '');
      await load();
      await loadApps();
    } catch (e) {
      error = errorText(e);
    } finally {
      installing = false;
      if (archiveInput) archiveInput.value = '';
    }
  }

  async function remove(plugin: Plugin) {
    if (!confirm(t.pluginConfirmRemove(name(plugin)))) return;
    try {
      await api.removePlugin(plugin.id);
      plugins = plugins.filter((p) => p.id !== plugin.id);
      error = '';
      await loadApps();
    } catch (e) {
      error = errorText(e);
    }
  }

  /** Puts a plugin the server sent back in its place. */
  function replace(saved: Plugin) {
    plugins = plugins.map((p) => (p.id === saved.id ? saved : p));
  }

  // While files are being downloaded, their progress is fetched now and then
  // (only storage and downloads: fields being edited stay as they are).
  const downloading = $derived(plugins.some((p) => p.download?.state === 'running'));
  $effect(() => {
    if (!downloading) return;
    const timer = setInterval(async () => {
      try {
        const fresh = await api.plugins();
        plugins = plugins.map((p) => {
          const now = fresh.find((f) => f.id === p.id);
          return now ? { ...p, storage: now.storage, download: now.download } : p;
        });
      } catch {
        // The next tick tries again.
      }
    }, 1500);
    return () => clearInterval(timer);
  });

  load();
</script>

<section class="card">
  <div class="title">
    <h2>{t.pluginsTitle}</h2>
    <div class="buttons">
      <button type="button" class="primary" onclick={() => (catalogOpen = true)}>{t.catalog}</button>
      <button type="button" disabled={installing} onclick={() => archiveInput?.click()}>{t.pluginInstall}</button>
    </div>
    <input
      bind:this={archiveInput}
      type="file"
      accept=".zip,application/zip"
      hidden
      onchange={(e) => install(e.currentTarget.files?.[0])}
    />
  </div>
  <p class="muted small">{t.pluginEnableNote} {t.pluginInstallHint}</p>
  {#if jobsMemory > 0}<p class="muted small">{t.pluginsJobsMemory(jobsMemory)}</p>{/if}
  {#if message}<p class="small">{message}</p>{/if}
  {#if error}<p class="error small">{error}</p>{/if}
  <ul>
    {#each plugins as plugin (plugin.id)}
      <li>
        <div class="head">
          <div class="what">
            <strong>{name(plugin)}</strong>
            <span class="muted small">
              {plugin.id}{plugin.manifest ? ` ${plugin.manifest.version}` : ''} · {t.pluginSource[plugin.source]}
            </span>
          </div>
          <span class="state {plugin.status.state}">{t.pluginState[plugin.status.state]}</span>
          <button type="button" onclick={() => (openId = plugin.id)}>{t.pluginConfigure}</button>
          {#if plugin.source !== 'built_in' && plugin.manifest}
            <button type="button" onclick={() => setEnabled(plugin, !plugin.enabled)}>
              {plugin.enabled ? t.pluginDisable : t.pluginEnable}
            </button>
          {/if}
          {#if plugin.source === 'installed'}
            <button type="button" class="danger" onclick={() => remove(plugin)}>{t.pluginRemove}</button>
          {/if}
        </div>
        {#if plugin.status.state === 'failed'}<p class="error small">{plugin.status.error}</p>{/if}
        {#if plugin.download?.state === 'failed'}
          <p class="error small">{t.pluginDownloadFailed(plugin.download.path, plugin.download.error)}</p>
        {/if}
      </li>
    {/each}
  </ul>
</section>

{#if catalogOpen}
  <PluginCatalog
    onchanged={async () => {
      await load();
      await loadApps();
    }}
    onclose={() => (catalogOpen = false)}
  />
{/if}

{#if opened}
  <PluginDialog
    plugin={opened}
    name={name(opened)}
    {maxMemory}
    onchanged={replace}
    onclose={() => (openId = null)}
  />
{/if}

<style>
  h2 {
    font-size: 1.05rem;
    margin: 0;
  }
  .title {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    margin-bottom: 4px;
  }
  .buttons {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }
  ul {
    list-style: none;
    margin: 8px 0 0;
    padding: 0;
  }
  li {
    padding: 10px 0;
    border-bottom: 1px solid var(--border-soft);
    display: grid;
    gap: 4px;
  }
  p {
    margin: 0;
  }
  .head {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 10px;
  }
  .what {
    flex: 1;
    min-width: 160px;
    display: grid;
  }
  .state {
    font-size: 0.85rem;
  }
  .state.running {
    color: var(--accent);
  }
  .state.disabled {
    color: var(--muted);
  }
  .state.failed {
    color: var(--danger);
  }
</style>
