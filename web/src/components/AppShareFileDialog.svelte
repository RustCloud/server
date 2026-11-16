<!--
  An app's page asks to share a file through it (`rustcloud.shareFile`) or
  for a public link to one (`rustcloud.shareLink`). The user picks the file
  (or the page gives one to upload, e.g. dropped into a chat) and agrees
  here, in the panel: the app itself gets no access to the user's files.
  Shares and links made so are marked with the app, which may end them later.
-->
<script lang="ts" module>
  export type ShareResult =
    | { kind: 'file'; id: string; name: string; size: number; mime_type: string | null; shares: string[] }
    | { kind: 'link'; id: string; url: string; name: string };
</script>

<script lang="ts">
  import { api, uploadFile, type FileNode } from '../lib/api';
  import { formatBytes } from '../lib/format';
  import { errorText, t } from '../lib/i18n';
  import Dialog from './Dialog.svelte';
  import FolderPicker from './FolderPicker.svelte';

  let {
    appId,
    appTitle,
    mode,
    people = [],
    upload,
    write: askedWrite = false,
    ondone,
  }: {
    appId: string;
    appTitle: string;
    /** `file`: with people; `link`: a public link. */
    mode: 'file' | 'link';
    people?: string[];
    upload?: File;
    write?: boolean;
    /** The result, or null if the user declined. */
    ondone: (result: ShareResult | null) => void;
  } = $props();

  let picked = $state<FileNode | null>(null);
  let folder = $state<FileNode | null>(null);
  let picking = $state<'file' | 'folder' | null>(null);
  let write = $state(false);
  let days = $state(7);
  let password = $state('');
  let busy = $state(false);
  let error = $state('');
  let rootId = $state('');

  $effect(() => {
    write = askedWrite;
    prepare();
  });

  /** The root, and for an upload the folder it goes into: "Attachments", made if missing. */
  async function prepare() {
    try {
      const root = await api.root();
      rootId = root.id;
      if (upload) {
        const existing = (await api.children(root.id)).find((n) => n.is_dir && n.name === t.attachmentsFolder);
        folder = existing ?? null;
      }
    } catch (e) {
      error = errorText(e);
    }
  }

  async function confirm() {
    busy = true;
    error = '';
    try {
      let node = picked;
      if (upload) {
        const target = folder ?? (await api.createFolder(rootId, t.attachmentsFolder));
        node = await uploadFile(target.id, upload, false, () => {}).done;
      }
      if (!node) return;
      if (mode === 'file') {
        const shares: string[] = [];
        for (const username of people) {
          const access = await api.grantAccess(node.id, { username, write, app: appId });
          shares.push(access.id);
        }
        ondone({ kind: 'file', id: node.id, name: node.name, size: node.size, mime_type: node.mime_type, shares });
      } else {
        const expires_at = days > 0 ? new Date(Date.now() + days * 86_400_000).toISOString() : null;
        const link = await api.createShare(node.id, { expires_at, password, app: appId });
        ondone({ kind: 'link', id: link.id, url: `${location.origin}/#/s/${link.token}`, name: node.name });
      }
    } catch (e) {
      error = errorText(e);
    } finally {
      busy = false;
    }
  }
</script>

<Dialog title={mode === 'file' ? t.shareFileAsk(appTitle) : t.shareLinkAsk(appTitle)} onclose={() => ondone(null)}>
  <div class="form">
    {#if upload}
      <p><strong>{upload.name}</strong> <span class="muted small">{formatBytes(upload.size)}</span></p>
      <p class="small">
        {t.shareSavedInto}: <strong>{folder?.name ?? t.attachmentsFolder}</strong>
        <button type="button" class="small" onclick={() => (picking = 'folder')}>{t.changeFolder}</button>
      </p>
    {:else}
      <p>
        {#if picked}<strong>{picked.name}</strong>{/if}
        <button type="button" class="small" onclick={() => (picking = 'file')}>{t.chooseFile}</button>
      </p>
    {/if}

    {#if mode === 'file'}
      <p>{t.shareWithPeople(people.join(', '))}</p>
      <label class="check small"><input type="checkbox" bind:checked={write} /> {t.shareAllowChanges}</label>
    {:else}
      <label class="small">
        {t.linkExpires}
        <select bind:value={days}>
          {#each [1, 7, 30] as n (n)}<option value={n}>{t.expiresInDays(n)}</option>{/each}
          <option value={0}>{t.never}</option>
        </select>
      </label>
      <label class="small">{t.linkPassword} <input type="password" bind:value={password} autocomplete="new-password" /></label>
    {/if}
    <p class="muted small">{t.shareAppNote}</p>
    {#if error}<p class="error small">{error}</p>{/if}
  </div>
  <footer>
    <button type="button" onclick={() => ondone(null)}>{t.cancel}</button>
    <button
      type="button"
      class="primary"
      disabled={busy || (!upload && !picked) || (mode === 'file' && people.length === 0)}
      onclick={confirm}
    >
      {t.share}
    </button>
  </footer>
</Dialog>

{#if picking && rootId}
  <FolderPicker
    title={picking === 'file' ? t.chooseFile : t.shareSavedInto}
    startId={rootId}
    exclude={[]}
    onchoose={(chosen) => {
      folder = chosen;
      picking = null;
    }}
    onchoosefile={picking === 'file'
      ? (file) => {
          picked = file;
          picking = null;
        }
      : undefined}
    onclose={() => (picking = null)}
  />
{/if}

<style>
  .form {
    display: grid;
    gap: 10px;
  }
  .form p {
    margin: 0;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 6px;
    color: var(--text);
  }
  footer {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 16px;
  }
</style>
