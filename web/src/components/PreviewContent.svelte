<!-- The preview of one file: an image, a player, or the beginning of a text file. -->
<script lang="ts">
  import type { FileNode } from '../lib/api';
  import { formatBytes } from '../lib/format';
  import { t } from '../lib/i18n';
  import { previewKind, TEXT_PREVIEW_BYTES } from '../lib/preview';

  let { node, url }: { node: FileNode; url: string } = $props();

  const kind = $derived(previewKind(node));
  let text = $state<string | null>(null);
  let textError = $state(false);

  $effect(() => {
    if (kind !== 'text') return;
    text = null;
    textError = false;
    // Only the beginning of large files: a Range request keeps the preview fast.
    const controller = new AbortController();
    fetch(url, { headers: { Range: `bytes=0-${TEXT_PREVIEW_BYTES - 1}` }, signal: controller.signal })
      .then((response) => (response.ok ? response.arrayBuffer() : Promise.reject(response.status)))
      .then((bytes) => (text = new TextDecoder().decode(bytes)))
      .catch(() => {
        if (!controller.signal.aborted) textError = true;
      });
    return () => controller.abort();
  });
</script>

{#if kind === 'image'}
  <img src={url} alt={node.name} />
{:else if kind === 'video'}
  <!-- svelte-ignore a11y_media_has_caption -->
  <video src={url} controls preload="metadata"></video>
{:else if kind === 'audio'}
  <audio src={url} controls preload="metadata"></audio>
{:else if kind === 'text'}
  {#if textError}
    <p class="error">{t.previewFailed}</p>
  {:else if text === null}
    <p class="muted">{t.loading}</p>
  {:else}
    <pre>{text}</pre>
    {#if node.size > TEXT_PREVIEW_BYTES}
      <p class="muted small">{t.previewTruncated(formatBytes(TEXT_PREVIEW_BYTES))}</p>
    {/if}
  {/if}
{:else}
  <p class="muted">{t.noPreview}</p>
{/if}

<style>
  img,
  video {
    display: block;
    max-width: 100%;
    max-height: 100%;
    margin: 0 auto;
    object-fit: contain;
  }
  audio {
    width: min(480px, 100%);
    margin: 40px auto;
    display: block;
  }
  pre {
    margin: 0;
    padding: 16px;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 8px;
    font: 0.85rem/1.5 var(--mono);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    text-align: left;
  }
</style>
