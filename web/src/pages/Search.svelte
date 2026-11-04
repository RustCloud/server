<script lang="ts">
  import { api, type FileNode } from '../lib/api';
  import { errorText, t } from '../lib/i18n';
  import { go } from '../lib/router.svelte';
  import FileList from '../components/FileList.svelte';
  import FilesTabs from '../components/FilesTabs.svelte';
  import Preview from '../components/Preview.svelte';
  import { previewKind } from '../lib/preview';

  let { query }: { query: string } = $props();

  let results = $state<FileNode[] | null>(null);
  let error = $state('');
  let previewing = $state<FileNode | null>(null);

  $effect(() => {
    results = null;
    api
      .search(query)
      .then((found) => (results = found))
      .catch((e) => (error = errorText(e)));
  });

  function open(node: FileNode) {
    if (node.is_dir) go('/files/' + node.id);
    else if (previewKind(node)) previewing = node;
    else location.href = api.contentUrl(node.id, true);
  }
</script>

<FilesTabs active="search" />
<h1>{t.searchResults(query)}</h1>
{#if error}<p class="error">{error}</p>{/if}
{#if results === null}
  <p class="muted">{t.loading}</p>
{:else if results.length === 0}
  <p class="muted">{t.nothingFound}</p>
{:else}
  <FileList nodes={results} onopen={open}>
    {#snippet actions(node)}
      {#if node.parent_id}
        <button type="button" class="small" onclick={() => go('/files/' + node.parent_id)}>{t.openFolder}</button>
      {/if}
    {/snippet}
  </FileList>
{/if}

{#if previewing && results}
  <Preview
    nodes={results}
    start={previewing}
    url={(node) => api.contentUrl(node.id)}
    downloadUrl={(node) => api.contentUrl(node.id, true)}
    onclose={() => (previewing = null)}
  />
{/if}
