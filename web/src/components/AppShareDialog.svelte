<!-- An app page asks to share a folder of its data (a calendar): who has it, add people, change or remove. -->
<script lang="ts">
  import { api, type AppShare } from '../lib/api';
  import { errorText, t } from '../lib/i18n';
  import Dialog from './Dialog.svelte';

  let {
    appId,
    appName,
    path,
    label,
    onclose,
  }: {
    appId: string;
    appName: string;
    /** The folder in the app's data, e.g. `calendars/work`. */
    path: string;
    /** What the user knows it as, e.g. the calendar's name. */
    label: string;
    onclose: () => void;
  } = $props();

  let shares = $state<AppShare[]>([]);
  let username = $state('');
  let write = $state(false);
  let error = $state('');
  let busy = $state(false);

  async function load() {
    try {
      const all = await api.appShares();
      shares = all.given.filter((share) => share.app_id === appId && share.path === path);
    } catch (e) {
      error = errorText(e);
    }
  }

  async function share(name: string, canWrite: boolean) {
    busy = true;
    try {
      await api.shareAppData({ app_id: appId, path, label, username: name, write: canWrite });
      error = '';
      await load();
      return true;
    } catch (e) {
      error = errorText(e);
      return false;
    } finally {
      busy = false;
    }
  }

  async function add(event: SubmitEvent) {
    event.preventDefault();
    if (await share(username, write)) username = '';
  }

  async function remove(share: AppShare) {
    try {
      await api.revokeAppShare(share.id);
      shares = shares.filter((s) => s.id !== share.id);
    } catch (e) {
      error = errorText(e);
    }
  }

  load();
</script>

<Dialog title={t.appShareTitle(label)} {onclose}>
  <p class="muted small">{t.appShareText(appName)}</p>
  {#if shares.length > 0}
    <ul>
      {#each shares as item (item.id)}
        <li>
          <div class="who">
            <span class="name">{item.user.display_name}</span>
            <span class="muted small">{item.user.username}</span>
          </div>
          <select value={item.write ? 'write' : 'read'} onchange={(e) => share(item.user.username, e.currentTarget.value === 'write')}>
            <option value="read">{t.appShareCanRead}</option>
            <option value="write">{t.appShareCanWrite}</option>
          </select>
          <button type="button" onclick={() => remove(item)}>{t.revoke}</button>
        </li>
      {/each}
    </ul>
  {:else}
    <p class="muted">{t.appShareNobody}</p>
  {/if}
  <form onsubmit={add}>
    <input bind:value={username} placeholder={t.appShareUsername} aria-label={t.appShareUsername} required />
    <select bind:value={write} aria-label={`${t.appShareCanRead} / ${t.appShareCanWrite}`}>
      <option value={false}>{t.appShareCanRead}</option>
      <option value={true}>{t.appShareCanWrite}</option>
    </select>
    <button type="submit" class="primary" disabled={busy}>{t.appShareAdd}</button>
  </form>
  {#if error}<p class="error small">{error}</p>{/if}
</Dialog>

<style>
  p {
    margin: 0 0 10px;
  }
  ul {
    list-style: none;
    margin: 0 0 12px;
    padding: 0;
  }
  li {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 0;
    border-bottom: 1px solid var(--border-soft);
  }
  .who {
    flex: 1;
    min-width: 0;
    display: grid;
  }
  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  form {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  form input {
    flex: 1;
    min-width: 140px;
  }
</style>
