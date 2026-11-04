<!--
  Full-window preview of files, with ← / → to go through the other
  previewable files of the same list. Esc closes it (native <dialog>).
-->
<script lang="ts">
  import { untrack } from 'svelte';
  import type { FileNode } from '../lib/api';
  import { formatBytes } from '../lib/format';
  import { t } from '../lib/i18n';
  import { previewKind } from '../lib/preview';
  import Icon from './Icon.svelte';
  import PreviewContent from './PreviewContent.svelte';

  let {
    nodes,
    start,
    url,
    downloadUrl,
    onclose,
  }: {
    /** The list the preview belongs to; only previewable files are visited. */
    nodes: FileNode[];
    start: FileNode;
    url: (node: FileNode) => string;
    downloadUrl: (node: FileNode) => string;
    onclose: () => void;
  } = $props();

  const files = $derived(nodes.filter((node) => previewKind(node) !== null));
  /** Starts at `start`, then follows the arrows. */
  let currentId = $state(untrack(() => start.id));
  const index = $derived(Math.max(0, files.findIndex((node) => node.id === currentId)));
  const current = $derived(files[index] ?? start);

  let dialog: HTMLDialogElement;
  $effect(() => {
    dialog.showModal();
  });

  function step(delta: number) {
    if (files.length) currentId = files[(index + delta + files.length) % files.length].id;
  }

  function onkeydown(event: KeyboardEvent) {
    if (event.target instanceof HTMLMediaElement) return; // arrows seek in players
    if (event.key === 'ArrowLeft') step(-1);
    if (event.key === 'ArrowRight') step(1);
  }
</script>

<dialog bind:this={dialog} class="preview" {onclose} {onkeydown} aria-label={current.name}>
  <header>
    <div class="title">
      <strong>{current.name}</strong>
      <span class="muted small">{formatBytes(current.size)}</span>
    </div>
    {#if files.length > 1}
      <button type="button" class="icon" onclick={() => step(-1)} aria-label={t.previous}><Icon name="left" /></button>
      <span class="muted small">{index + 1} / {files.length}</span>
      <button type="button" class="icon" onclick={() => step(1)} aria-label={t.next}><Icon name="right" /></button>
    {/if}
    <a class="button small" href={downloadUrl(current)} download><Icon name="download" /> {t.download}</a>
    <button type="button" class="icon" onclick={() => dialog.close()} aria-label={t.close}><Icon name="close" /></button>
  </header>
  <div class="content">
    {#key current.id}
      <PreviewContent node={current} url={url(current)} />
    {/key}
  </div>
</dialog>

<style>
  .preview {
    width: 100vw;
    height: 100vh;
    max-width: none;
    max-height: none;
    margin: 0;
    padding: 0;
    border: none;
    background: var(--surface);
    color: var(--text);
  }
  .preview[open] {
    display: flex;
    flex-direction: column;
  }
  header {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 10px 16px;
    border-bottom: 1px solid var(--border);
  }
  .title {
    flex: 1;
    min-width: 0;
    display: flex;
    gap: 10px;
    align-items: baseline;
  }
  .title strong {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  header .icon {
    padding: 6px;
    border-radius: 6px;
  }
  header .icon:hover {
    background: var(--hover);
  }
  .content {
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding: 16px;
    background: var(--bg);
  }
</style>
