<!-- Create, copy and delete public links of one file or folder. -->
<script lang="ts">
  import { api, type FileNode, type Share } from '../lib/api';
  import { formatDate } from '../lib/format';
  import { errorText, locale, t } from '../lib/i18n';
  import Dialog from './Dialog.svelte';
  import ShareLink from './ShareLink.svelte';

  let { node, onclose }: { node: FileNode; onclose: () => void } = $props();

  let links = $state<Share[]>([]);
  let expires = $state('');
  let error = $state('');

  async function refresh() {
    try {
      links = (await api.shares()).filter((s) => s.node.id === node.id);
    } catch (e) {
      error = errorText(e);
    }
  }

  async function create(event: SubmitEvent) {
    event.preventDefault();
    try {
      // <input type="datetime-local"> gives local time without a zone.
      const expiresAt = expires ? new Date(expires).toISOString() : null;
      await api.createShare(node.id, expiresAt);
      expires = '';
      error = '';
      await refresh();
    } catch (e) {
      error = errorText(e);
    }
  }

  async function remove(share: Share) {
    await api.deleteShare(share.id);
    await refresh();
  }

  refresh();
</script>

<Dialog title={`${t.share}: ${node.name}`} {onclose}>
  <form onsubmit={create}>
    <label>
      {t.linkExpires}
      <input type="datetime-local" bind:value={expires} />
    </label>
    <p class="muted small">{t.linkExpiresHint}</p>
    <button type="submit" class="primary">{t.createLink}</button>
  </form>
  {#if error}<p class="error">{error}</p>{/if}

  {#if links.length}
    <h3>{t.existingLinks}</h3>
    {#each links as link (link.id)}
      <div class="link">
        <ShareLink token={link.token} />
        <div class="meta">
          <span class="muted small">
            {t.linkExpires}: {link.expires_at ? formatDate(link.expires_at, locale) : t.never}
          </span>
          <button type="button" class="danger small" onclick={() => remove(link)}>{t.deleteLink}</button>
        </div>
      </div>
    {/each}
  {/if}
</Dialog>

<style>
  form {
    display: grid;
    gap: 8px;
    justify-items: start;
  }
  h3 {
    font-size: 0.95rem;
    margin: 20px 0 8px;
  }
  .link {
    padding: 10px 0;
    border-top: 1px solid var(--border-soft);
  }
  .meta {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-top: 6px;
  }
</style>
