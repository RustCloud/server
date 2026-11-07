<!-- What other people shared with the user, directly or through a group. -->
<script lang="ts">
  import { api, type FileNode, type Received } from '../lib/api';
  import { errorText, t } from '../lib/i18n';
  import { go } from '../lib/router.svelte';
  import FileList from '../components/FileList.svelte';
  import FilesTabs from '../components/FilesTabs.svelte';

  let received = $state<Received[] | null>(null);
  let error = $state('');

  async function refresh() {
    try {
      received = await api.sharedWithMe();
    } catch (e) {
      error = errorText(e);
    }
  }

  function open(node: FileNode) {
    if (node.is_dir) go(`/files/${node.id}`);
    else location.href = api.contentUrl(node.id, true);
  }

  async function leave(item: Received) {
    try {
      await api.removeAccess(item.id);
    } catch (e) {
      error = errorText(e);
    }
    await refresh();
  }

  refresh();
</script>

<FilesTabs active="shared" />
{#if error}<p class="error">{error}</p>{/if}
{#if received === null}
  <p class="muted">{t.loading}</p>
{:else if received.length === 0}
  <p class="muted">{t.nothingShared}</p>
{:else}
  <FileList nodes={received.map((item) => item.node)} onopen={open} thumbnail={(node) => api.thumbnailUrl(node)}>
    {#snippet actions(node)}
      {@const item = received?.find((r) => r.node.id === node.id)}
      {#if item}
        <span class="muted small">
          {item.owner.display_name}{item.group ? ` · ${item.group.name}` : ''}{item.write ? '' : ` · ${t.readOnlyShare}`}
        </span>
        {#if !item.group}
          <button type="button" class="small" onclick={() => leave(item)}>{t.leaveShare}</button>
        {/if}
      {/if}
    {/snippet}
  </FileList>
{/if}
