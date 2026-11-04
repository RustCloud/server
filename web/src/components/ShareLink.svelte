<!-- A public link in a read-only field with a "copy" button. -->
<script lang="ts">
  import { t } from '../lib/i18n';
  import { server } from '../lib/server.svelte';

  let { token }: { token: string } = $props();

  const url = $derived(`${server.publicUrl}/#/s/${token}`);
  let input: HTMLInputElement;
  let copied = $state(false);

  async function copy() {
    try {
      // The clipboard API exists only on HTTPS and localhost.
      await navigator.clipboard.writeText(url);
    } catch {
      input.select();
      document.execCommand('copy');
    }
    copied = true;
    setTimeout(() => (copied = false), 1500);
  }
</script>

<div class="share-link">
  <input bind:this={input} readonly value={url} onfocus={() => input.select()} />
  <button type="button" onclick={copy}>{copied ? t.linkCopied : t.copyLink}</button>
</div>

<style>
  .share-link {
    display: flex;
    gap: 6px;
  }
  input {
    flex: 1;
    min-width: 0;
    font-family: var(--mono);
    font-size: 0.8rem;
  }
</style>
