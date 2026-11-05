<!-- A modal dialog on top of the native <dialog> element (Esc and focus handling for free). -->
<script lang="ts">
  import type { Snippet } from 'svelte';
  import { t } from '../lib/i18n';

  let {
    title,
    onclose,
    children,
    wide = false,
    large = false,
  }: { title: string; onclose: () => void; children: Snippet; wide?: boolean; large?: boolean } = $props();

  let dialog: HTMLDialogElement;

  $effect(() => {
    dialog.showModal();
  });
</script>

<dialog bind:this={dialog} {onclose} aria-label={title} class:wide class:large>
  <header>
    <h2>{title}</h2>
    <button type="button" class="icon" aria-label={t.close} onclick={() => dialog.close()}>×</button>
  </header>
  {@render children()}
</dialog>

<style>
  dialog {
    width: min(440px, calc(100vw - 32px));
    border: 1px solid var(--border);
    border-radius: 12px;
    padding: 20px;
    background: var(--surface);
    color: var(--text);
    box-shadow: 0 20px 60px rgb(0 0 0 / 0.25);
  }
  dialog.wide {
    width: min(720px, calc(100vw - 32px));
  }
  dialog.large {
    width: min(880px, calc(100vw - 32px));
  }
  dialog::backdrop {
    background: rgb(0 0 0 / 0.35);
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 12px;
  }
  h2 {
    margin: 0;
    font-size: 1.1rem;
  }
  .icon {
    font-size: 1.4rem;
    line-height: 1;
  }
</style>
