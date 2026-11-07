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
  let password = $state('');
  let allowUpload = $state(false);
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
      await api.createShare(node.id, {
        expires_at: expiresAt,
        password: password || undefined,
        allow_upload: node.is_dir ? allowUpload : undefined,
      });
      expires = password = '';
      allowUpload = false;
      error = '';
      await refresh();
    } catch (e) {
      error = errorText(e);
    }
  }

  async function change(share: Share, options: Parameters<typeof api.updateShare>[1]) {
    try {
      await api.updateShare(share.id, options);
      error = '';
    } catch (e) {
      error = errorText(e);
    }
    await refresh();
  }

  function changePassword(share: Share) {
    const newPassword = prompt(t.linkNewPassword);
    if (newPassword !== null) change(share, { password: newPassword });
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
    <label>
      {t.linkPassword}
      <input type="password" autocomplete="new-password" bind:value={password} />
    </label>
    {#if node.is_dir}
      <label class="check"><input type="checkbox" bind:checked={allowUpload} /> {t.linkAllowUpload}</label>
    {/if}
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
            {#if link.has_password}· {t.linkWithPassword}{/if}
            {#if link.allow_upload}· {t.linkUploads}{/if}
          </span>
          <span class="buttons">
            {#if node.is_dir}
              <label class="check small">
                <input
                  type="checkbox"
                  checked={link.allow_upload}
                  onchange={(e) => change(link, { allow_upload: e.currentTarget.checked })}
                />
                {t.upload}
              </label>
            {/if}
            <button type="button" class="small" onclick={() => changePassword(link)}>{t.linkChangePassword}</button>
            <button type="button" class="danger small" onclick={() => remove(link)}>{t.deleteLink}</button>
          </span>
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
    gap: 8px;
    flex-wrap: wrap;
    margin-top: 6px;
  }
  .buttons {
    display: flex;
    gap: 6px;
    align-items: center;
  }
  .check {
    display: flex;
    gap: 6px;
    align-items: center;
  }
</style>
