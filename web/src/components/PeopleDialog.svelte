<!-- Whom a file or folder is shared with (people and groups), and sharing it with more. -->
<script lang="ts">
  import { api, type Access, type FileNode } from '../lib/api';
  import { errorText, t } from '../lib/i18n';
  import Dialog from './Dialog.svelte';

  let { node, onclose }: { node: FileNode; onclose: () => void } = $props();

  let shares = $state<Access[]>([]);
  let groups = $state<{ id: string; name: string }[]>([]);
  /** A username, or `group:<id>`. */
  let whom = $state('');
  let groupId = $state('');
  let write = $state(false);
  let error = $state('');

  async function refresh() {
    try {
      shares = await api.nodeAccess(node.id);
    } catch (e) {
      error = errorText(e);
    }
  }

  async function add(event: SubmitEvent) {
    event.preventDefault();
    const to = groupId ? { group_id: groupId, write } : { username: whom.trim(), write };
    if (!groupId && !whom.trim()) return;
    try {
      await api.grantAccess(node.id, to);
      whom = groupId = '';
      write = false;
      error = '';
    } catch (e) {
      error = errorText(e);
    }
    await refresh();
  }

  async function change(share: Access, canWrite: boolean) {
    try {
      await api.changeAccess(share.id, canWrite);
    } catch (e) {
      error = errorText(e);
    }
    await refresh();
  }

  async function remove(share: Access) {
    try {
      await api.removeAccess(share.id);
    } catch (e) {
      error = errorText(e);
    }
    await refresh();
  }

  refresh();
  api
    .myGroups()
    .then((mine) => (groups = mine))
    .catch(() => {});
</script>

<Dialog title={`${t.people}: ${node.name}`} {onclose}>
  <form onsubmit={add}>
    <div class="row">
      <input placeholder={t.username} bind:value={whom} disabled={!!groupId} aria-label={t.username} />
      {#if groups.length}
        <select bind:value={groupId} aria-label={t.group}>
          <option value="">{t.orGroup}</option>
          {#each groups as group (group.id)}
            <option value={group.id}>{group.name}</option>
          {/each}
        </select>
      {/if}
    </div>
    <label class="check"><input type="checkbox" bind:checked={write} /> {t.canChange}</label>
    <button type="submit" class="primary">{t.shareWith}</button>
    <p class="muted small">{t.accessHint}</p>
  </form>
  {#if error}<p class="error">{error}</p>{/if}

  {#if shares.length}
    <h3>{t.sharedWith}</h3>
    {#each shares as share (share.id)}
      <div class="share">
        <span>{share.group ? `${t.group}: ${share.group.name}` : share.user?.username}</span>
        <label class="check small">
          <input type="checkbox" checked={share.write} onchange={(e) => change(share, e.currentTarget.checked)} />
          {t.canChange}
        </label>
        <button type="button" class="danger small" onclick={() => remove(share)}>{t.stopSharing}</button>
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
  .row {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }
  .check {
    display: flex;
    gap: 6px;
    align-items: center;
  }
  h3 {
    font-size: 0.95rem;
    margin: 20px 0 8px;
  }
  .share {
    display: flex;
    gap: 12px;
    align-items: center;
    justify-content: space-between;
    padding: 8px 0;
    border-top: 1px solid var(--border-soft);
  }
</style>
