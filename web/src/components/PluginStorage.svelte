<!--
  A plugin's own storage on the server: its size, its files, "Clear", and
  how the downloads of its manifest go ("Try again" after a failure).
-->
<script lang="ts">
  import { api, type Plugin, type PluginStorageFile } from '../lib/api';
  import { formatBytes, formatDate } from '../lib/format';
  import { errorText, locale, t } from '../lib/i18n';

  let { plugin, name, onchanged }: { plugin: Plugin; name: string; onchanged: (saved: Plugin) => void } = $props();

  /** The files, while the list is open. */
  let files = $state<PluginStorageFile[] | null>(null);
  let error = $state('');

  const download = $derived(plugin.download);
  const shown = $derived(plugin.storage.files > 0 || !!download || !!plugin.manifest?.downloads?.length);

  function progress(done: number, total: number | null): string {
    if (!total) return formatBytes(done);
    return `${Math.floor((done * 100) / total)}% (${formatBytes(done)} / ${formatBytes(total)})`;
  }

  async function toggleFiles() {
    if (files) {
      files = null;
      return;
    }
    try {
      files = await api.pluginStorage(plugin.id);
      error = '';
    } catch (e) {
      error = errorText(e);
    }
  }

  async function run(action: () => Promise<Plugin>) {
    try {
      onchanged(await action());
      files = null;
      error = '';
    } catch (e) {
      error = errorText(e);
    }
  }

  function clear() {
    if (confirm(t.pluginConfirmClear(name))) run(() => api.clearPluginStorage(plugin.id));
  }
</script>

{#if shown}
  <div class="storage small">
    <span class="muted">{t.pluginStorage(formatBytes(plugin.storage.bytes), plugin.storage.files)}</span>
    {#if plugin.storage.files > 0}
      <button type="button" class="link" onclick={toggleFiles}>{t.pluginStorageFiles}</button>
      <button type="button" class="link" onclick={clear}>{t.pluginStorageClear}</button>
    {/if}
  </div>
  {#if download?.state === 'running'}
    <p class="small">{t.pluginDownloading(download.path, progress(download.done, download.total), download.left)}</p>
  {:else if download?.state === 'failed'}
    <p class="small error">
      {t.pluginDownloadFailed(download.path, download.error)}
      <button type="button" class="link" onclick={() => run(() => api.retryPluginDownloads(plugin.id))}>
        {t.pluginRetry}
      </button>
    </p>
  {/if}
  {#if files}
    <ul class="small">
      {#each files as file (file.path)}
        <li>
          <span>{file.path}</span>
          <span class="muted">{formatBytes(file.size)} · {formatDate(file.updated_at, locale)}</span>
        </li>
      {/each}
    </ul>
  {/if}
  {#if error}<p class="error small">{error}</p>{/if}
{/if}

<style>
  .storage {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 10px;
  }
  p {
    margin: 0;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0 0 0 12px;
    display: grid;
    gap: 2px;
  }
  li {
    display: flex;
    gap: 10px;
    justify-content: space-between;
    overflow-wrap: anywhere;
  }
</style>
