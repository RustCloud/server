<!--
  One plugin, over the Plugins tab, in three tabs:
  - Settings: the plugin's own server settings (a model, the editor's address);
  - Status: whether it runs, what it tells about itself on its status path,
    its storage and downloads, its background jobs;
  - System: what the server gives it — what it may reach and its multiples
    of memory, time and job batches.
-->
<script lang="ts">
  import { untrack } from 'svelte';
  import { api, type Plugin, type Scale, type StatusItem } from '../lib/api';
  import { loadApps } from '../lib/apps.svelte';
  import { errorText, locale, t } from '../lib/i18n';
  import Dialog from './Dialog.svelte';
  import PluginJobs from './PluginJobs.svelte';
  import PluginSettingsForm from './PluginSettingsForm.svelte';
  import PluginStorage from './PluginStorage.svelte';

  let {
    plugin,
    name,
    maxMemory,
    onchanged,
    onclose,
  }: {
    plugin: Plugin;
    name: string;
    /** The most memory one plugin may get, as a multiple (`[plugins] max_memory`). */
    maxMemory: number;
    onchanged: (saved: Plugin) => void;
    onclose: () => void;
  } = $props();

  type Tab = 'settings' | 'status' | 'system';
  const hasSettings = $derived(!!plugin.manifest?.server_settings?.length);
  /** Built-in apps are part of the server: nothing of theirs is set here. */
  const configurable = $derived(plugin.source !== 'built_in' && !!plugin.manifest);
  let tab = $state<Tab>(untrack(() => (plugin.manifest?.server_settings?.length ? 'settings' : 'status')));

  // ----- Status -----

  let report = $state<StatusItem[] | null>(null);
  let reportError = $state('');
  let reportLoading = $state(false);
  const statusPath = $derived(plugin.manifest?.status);

  async function loadReport() {
    if (!statusPath || plugin.status.state !== 'running') return;
    reportLoading = true;
    try {
      report = await api.appStatus(plugin.id, statusPath);
      reportError = '';
    } catch (e) {
      report = null;
      reportError = errorText(e);
    }
    reportLoading = false;
  }

  $effect(() => {
    if (tab === 'status') untrack(loadReport);
  });

  function itemText(item: StatusItem): string {
    if (typeof item.text === 'string') return item.text;
    return item.text[locale] ?? item.text.en ?? Object.values(item.text)[0] ?? '';
  }

  // ----- System -----

  const STEPS = [1, 2, 4, 8, 16, 32, 64];
  /** The multiples being edited: they start from what the plugin gets now. */
  let scale = $state<Scale>(untrack(() => ({ ...(plugin.limits ?? plugin.effective.scale) })));
  let systemError = $state('');
  let systemMessage = $state('');

  /** What the plugin may ask users for, the addresses it takes and the servers it reaches. */
  const rights = $derived.by(() => {
    const manifest = plugin.manifest;
    const permissions = manifest?.permissions;
    const files = permissions?.files ?? 'none';
    const parts = [t.pluginFiles[files] + (files !== 'none' && permissions?.files_write ? `, ${t.pluginFilesWrite}` : '')];
    const addresses = Object.keys(manifest?.well_known ?? {});
    if (addresses.length) parts.push(`${t.pluginAddresses}: ${addresses.map((a) => `/.well-known/${a}`).join(', ')}`);
    const network = permissions?.network ?? [];
    if (network.length) parts.push(`${t.pluginNetwork}: ${network.join(', ')}`);
    const uses = permissions?.use ?? [];
    if (uses.length) parts.push(`${t.pluginUses}: ${uses.map((tool) => `use.${tool}`).join(', ')}`);
    const frames = permissions?.frame ?? [];
    if (frames.length) parts.push(`${t.pluginFrames}: ${frames.join(', ')}`);
    if (permissions?.images === 'any') parts.push(t.pluginImagesAny);
    for (const proxy of manifest?.proxy ?? []) parts.push(t.pluginProxy(proxy.paths, proxy.target));
    const asks = manifest?.limits;
    if (asks && (asks.memory > 1 || asks.time > 1 || asks.workers > 1)) parts.push(t.pluginAsks(asks));
    return parts;
  });

  async function saveLimits(limits: Scale | null) {
    try {
      const saved = await api.configurePlugin(plugin.id, { enabled: plugin.enabled, limits });
      onchanged(saved);
      scale = { ...(saved.limits ?? saved.effective.scale) };
      systemError = '';
      systemMessage = t.pluginSettingsSaved;
      await loadApps();
    } catch (e) {
      systemError = errorText(e);
      systemMessage = '';
    }
  }
</script>

<Dialog title={name} {onclose} wide>
  <nav class="tabs">
    <button type="button" class:active={tab === 'settings'} onclick={() => (tab = 'settings')}>
      {t.pluginTabSettings}
    </button>
    <button type="button" class:active={tab === 'status'} onclick={() => (tab = 'status')}>{t.pluginTabStatus}</button>
    {#if configurable}
      <button type="button" class:active={tab === 'system'} onclick={() => (tab = 'system')}>
        {t.pluginTabSystem}
      </button>
    {/if}
  </nav>

  <div class="body">
    {#if tab === 'settings'}
      {#if hasSettings}
        <PluginSettingsForm {plugin} onsaved={onchanged} />
      {:else}
        <p class="muted">{t.pluginNoSettings}</p>
      {/if}
    {:else if tab === 'status'}
      <p>
        <span class="state {plugin.status.state}">{t.pluginState[plugin.status.state]}</span>
        <span class="muted small">
          {plugin.id}{plugin.manifest ? ` ${plugin.manifest.version}` : ''} · {t.pluginSource[plugin.source]}
        </span>
      </p>
      {#if plugin.status.state === 'failed'}<p class="error small">{plugin.status.error}</p>{/if}

      {#if statusPath && plugin.status.state === 'running'}
        <div class="report">
          <div class="row">
            <h3>{t.pluginReport}</h3>
            <button type="button" class="small" disabled={reportLoading} onclick={loadReport}>
              {t.journalRefresh}
            </button>
          </div>
          {#if reportError}
            <p class="error small">{t.pluginReportFailed(reportError)}</p>
          {:else if report?.length}
            <ul>
              {#each report as item, i (i)}
                <li class={item.level}><span class="dot" aria-hidden="true"></span>{itemText(item)}</li>
              {/each}
            </ul>
          {:else if report}
            <p class="muted small">{t.pluginReportEmpty}</p>
          {:else}
            <p class="muted small">{t.loading}</p>
          {/if}
        </div>
      {/if}

      <PluginStorage {plugin} {name} onchanged={onchanged} />
      <PluginJobs {plugin} />
    {:else if tab === 'system' && configurable}
      <h3>{t.pluginRights}</h3>
      <ul class="rights small">
        {#each rights as right (right)}<li>{right}</li>{/each}
      </ul>

      <h3>{t.pluginAdvanced}</h3>
      <p class="muted small">{t.pluginResources(plugin.effective, plugin.limits !== null)}</p>
      <form
        class="limits"
        onsubmit={(e) => {
          e.preventDefault();
          saveLimits(scale);
        }}
      >
        <label class="small">
          {t.pluginMemory}
          <select bind:value={scale.memory}>
            {#each STEPS.filter((step) => step <= maxMemory) as step (step)}
              <option value={step}>×{step} · {t.pluginMemoryStep(step)}</option>
            {/each}
          </select>
        </label>
        <label class="small">
          {t.pluginTime}
          <select bind:value={scale.time}>
            {#each STEPS as step (step)}
              <option value={step}>×{step} · {t.pluginTimeStep(step)}</option>
            {/each}
          </select>
        </label>
        {#if plugin.manifest?.jobs?.length}
          <label class="small">
            {t.pluginWorkers}
            <input type="number" min="1" max="16" bind:value={scale.workers} />
          </label>
        {/if}
        <div class="row">
          <button type="submit" class="primary">{t.save}</button>
          <button type="button" onclick={() => saveLimits(null)}>{t.pluginAsManifest}</button>
          {#if systemMessage}<span class="small">{systemMessage}</span>{/if}
        </div>
        <p class="muted small">{t.pluginAdvancedHint}</p>
        {#if systemError}<p class="error small">{systemError}</p>{/if}
      </form>
    {/if}
  </div>
</Dialog>

<style>
  .tabs button {
    border: none;
    border-bottom: 2px solid transparent;
    border-radius: 0;
    background: none;
    padding: 8px 14px;
    color: var(--muted);
    white-space: nowrap;
  }
  .tabs button:hover {
    background: none;
    color: var(--text);
  }
  .tabs button.active {
    color: var(--accent);
    border-bottom-color: var(--accent);
  }
  .body {
    display: grid;
    gap: 10px;
    max-height: min(65vh, 640px);
    overflow-y: auto;
  }
  p {
    margin: 0;
  }
  h3 {
    font-size: 0.95rem;
    margin: 4px 0 0;
  }
  .row {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 8px;
  }
  .report .row {
    justify-content: space-between;
  }
  .report ul {
    list-style: none;
    margin: 6px 0 0;
    padding: 0;
    display: grid;
    gap: 4px;
  }
  .report li {
    display: flex;
    align-items: baseline;
    gap: 8px;
    overflow-wrap: anywhere;
  }
  .dot {
    flex: none;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--muted);
  }
  .ok .dot {
    background: var(--ok);
  }
  .warning .dot {
    background: var(--accent);
  }
  .error .dot {
    background: var(--danger);
  }
  li.error {
    color: var(--danger);
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
  .rights {
    margin: 0;
    padding-left: 18px;
    display: grid;
    gap: 2px;
  }
  .limits {
    display: grid;
    gap: 8px;
  }
  .limits label {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .limits input {
    width: 70px;
  }
</style>
