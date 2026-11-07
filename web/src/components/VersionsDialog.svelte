<!-- Earlier versions of a file: download one or make it current again. -->
<script lang="ts">
  import { api, type FileNode, type Versions } from '../lib/api';
  import { formatBytes, formatDate } from '../lib/format';
  import { errorText, locale, t } from '../lib/i18n';
  import Dialog from './Dialog.svelte';

  let { node, onclose, onrestored }: { node: FileNode; onclose: () => void; onrestored: () => void } = $props();

  let current = $state<FileNode | null>(null);
  let list = $state<Versions | null>(null);
  let busy = $state(false);
  let error = $state('');

  async function refresh() {
    try {
      current = (await api.details(node.id)).node;
      list = await api.versions(node.id);
    } catch (e) {
      error = errorText(e);
    }
  }

  async function restore(versionId: string) {
    busy = true;
    try {
      await api.restoreVersion(node.id, versionId);
      error = '';
      onrestored();
      await refresh();
    } catch (e) {
      error = errorText(e);
    }
    busy = false;
  }

  refresh();
</script>

<Dialog title={`${t.versions}: ${node.name}`} {onclose}>
  {#if error}<p class="error">{error}</p>{/if}
  {#if current && list}
    <ul>
      <li>
        <span>{formatDate(current.modified_at, locale)}</span>
        <span class="muted small">{formatBytes(current.size)}</span>
        <span class="current muted small">{t.currentVersion}</span>
      </li>
      {#each list.versions as version (version.id)}
        <li>
          <span>{formatDate(version.modified_at, locale)}</span>
          <span class="muted small">{formatBytes(version.size)}</span>
          <span class="actions">
            <a class="button small" href={api.versionUrl(node.id, version.id)} download>{t.download}</a>
            <button type="button" class="small" disabled={busy} onclick={() => restore(version.id)}>{t.restore}</button>
          </span>
        </li>
      {/each}
    </ul>
    {#if !list.versions.length}<p class="muted small">{t.noVersions}</p>{/if}
    <p class="muted small">{t.versionsKept(list.keep, list.days)} {t.versionsQuota}</p>
  {:else if !error}
    <p class="muted">{t.loading}</p>
  {/if}
</Dialog>

<style>
  ul {
    list-style: none;
    margin: 0 0 12px;
    padding: 0;
  }
  li {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 0;
    border-top: 1px solid var(--border-soft);
  }
  li:first-child {
    border-top: none;
  }
  .current,
  .actions {
    margin-left: auto;
    display: flex;
    gap: 6px;
  }
</style>
