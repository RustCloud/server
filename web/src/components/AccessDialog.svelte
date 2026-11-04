<!-- An app asks for standing access to a folder: create one or pick one, then grant it. -->
<script lang="ts">
  import { api, ApiError, type FileNode, type Grant } from '../lib/api';
  import { errorText, t } from '../lib/i18n';
  import Dialog from './Dialog.svelte';
  import FolderPicker from './FolderPicker.svelte';

  let {
    appId,
    appName,
    folderName,
    write,
    ongranted,
    onclose,
  }: {
    appId: string;
    appName: string;
    /** The folder the app suggests creating, e.g. `Notes`. */
    folderName: string;
    /** Whether the app asks to change files, not only read them. */
    write: boolean;
    ongranted: (grant: Grant) => void;
    /** Closed without granting. */
    onclose: () => void;
  } = $props();

  let choice = $state<'create' | 'pick'>('create');
  let picked = $state<FileNode | null>(null);
  let picking = $state(false);
  let error = $state('');
  let busy = $state(false);
  let granted = false;
  let rootId = $state('');
  api.root().then((root) => (rootId = root.id), (e) => (error = errorText(e)));

  /** The suggested folder in the root: created, or the existing one with that name. */
  async function suggestedFolder(): Promise<FileNode> {
    const root = await api.root();
    try {
      return await api.createFolder(root.id, folderName);
    } catch (e) {
      if (!(e instanceof ApiError) || e.code !== 'NAME_TAKEN') throw e;
      const existing = (await api.children(root.id)).find((n) => n.is_dir && n.name === folderName);
      if (!existing) throw e;
      return existing;
    }
  }

  async function allow(event: SubmitEvent) {
    event.preventDefault();
    busy = true;
    try {
      const folder = choice === 'create' ? await suggestedFolder() : picked;
      if (!folder) return;
      const grant = await api.createGrant({ app_id: appId, node_id: folder.id, write, kind: 'standing' });
      granted = true;
      ongranted(grant);
    } catch (e) {
      error = errorText(e);
    } finally {
      busy = false;
    }
  }

  function closed() {
    if (!granted) onclose();
  }
</script>

{#if picking}
  <FolderPicker
    title={t.accessPickTitle}
    startId={picked?.id ?? rootId}
    exclude={[]}
    onchoose={(folder) => {
      picked = folder;
      choice = 'pick';
      picking = false;
    }}
    onclose={() => (picking = false)}
  />
{:else}
  <Dialog title={t.accessRequestTitle(appName)} onclose={closed}>
    <form onsubmit={allow}>
      <p>{t.accessRequestText(appName)}</p>
      <label class="option">
        <input type="radio" bind:group={choice} value="create" />
        {t.accessCreateFolder(folderName)}
      </label>
      <label class="option">
        <input
          type="radio"
          bind:group={choice}
          value="pick"
          disabled={!rootId}
          onclick={() => !picked && (picking = true)}
        />
        {picked ? t.accessPickedFolder(picked.name) : t.accessPickFolder}
      </label>
      {#if picked}
        <button type="button" class="link" onclick={() => (picking = true)}>{t.accessPickAnother}</button>
      {/if}
      <p class="muted small">{write ? t.accessWillWrite : t.accessWillRead}</p>
      {#if error}<p class="error small">{error}</p>{/if}
      <div class="buttons">
        <button type="submit" class="primary" disabled={busy || (choice === 'pick' && !picked)}>{t.accessAllow}</button>
      </div>
    </form>
  </Dialog>
{/if}

<style>
  form {
    display: grid;
    gap: 10px;
  }
  p {
    margin: 0;
  }
  .option {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .link {
    justify-self: start;
    background: none;
    border: 0;
    padding: 0 0 0 26px;
    color: var(--accent);
    cursor: pointer;
  }
  .buttons {
    display: flex;
    justify-content: flex-end;
  }
</style>
