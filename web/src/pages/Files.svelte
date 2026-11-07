<!-- The file browser: one folder with breadcrumbs, uploads and all file actions. -->
<script lang="ts">
  import { api, uploadFile, type FileNode } from '../lib/api';
  import { openersFor } from '../lib/apps';
  import { apps, openInApp } from '../lib/apps.svelte';
  import { copyName } from '../lib/format';
  import { appKinds, coreKinds, freeName, type NewFileKind } from '../lib/newfiles';
  import { previewKind } from '../lib/preview';
  import { entriesFromDrop, entriesFromInput, FolderMaker, type UploadEntry } from '../lib/upload';
  import { errorText, locale, t } from '../lib/i18n';
  import { go, href } from '../lib/router.svelte';
  import Dialog from '../components/Dialog.svelte';
  import DropMenu from '../components/DropMenu.svelte';
  import { isShown } from '../lib/prefs.svelte';
  import FileList from '../components/FileList.svelte';
  import FilesTabs from '../components/FilesTabs.svelte';
  import FolderPicker from '../components/FolderPicker.svelte';
  import Icon from '../components/Icon.svelte';
  import Preview from '../components/Preview.svelte';
  import ShareDialog from '../components/ShareDialog.svelte';
  import VersionsDialog from '../components/VersionsDialog.svelte';
  import UploadPanel, { type UploadItem } from '../components/UploadPanel.svelte';

  /** Folder to show; the user's root folder when undefined, or with `trash` the trash. */
  let { folderId, trash = false }: { folderId?: string; trash?: boolean } = $props();

  /** The hidden folder in the root where deleted things go. */
  const TRASH = '.trash';

  let folder = $state<FileNode | null>(null);
  let path = $state<FileNode[]>([]);
  let items = $state<FileNode[]>([]);
  let selected = $state<string[]>([]);
  let error = $state('');
  let dragging = $state(false);
  let uploads = $state<UploadItem[]>([]);
  let nextUploadId = 0;

  type DialogState =
    | { kind: 'newFolder' }
    | { kind: 'newFile'; type: NewFileKind }
    | { kind: 'rename'; node: FileNode }
    | { kind: 'move' | 'copy'; nodes: FileNode[] }
    | { kind: 'share'; node: FileNode }
    | { kind: 'versions'; node: FileNode }
    | null;
  let dialog = $state<DialogState>(null);
  let previewing = $state<FileNode | null>(null);
  let nameInput = $state('');

  /** Items dragged onto a folder, waiting for "move or copy?". */
  let dropMenu = $state<{ x: number; y: number; nodes: FileNode[]; target: FileNode } | null>(null);

  let fileInput = $state<HTMLInputElement>();
  let folderInput = $state<HTMLInputElement>();

  /** Drag data type for rows dragged inside the app (their ids, as JSON). */
  const NODES_TYPE = 'application/x-rustcloud-nodes';
  /** Uploads running at the same time; the rest wait in a queue. */
  const PARALLEL_UPLOADS = 3;
  let runningUploads = 0;
  const waitingUploads: (() => void)[] = [];

  async function load(id: string | undefined, toTrash: boolean) {
    try {
      if (toTrash) id = (await api.trash()).id;
      if (id) {
        const details = await api.details(id);
        folder = details.node;
        path = details.path;
      } else {
        folder = await api.root();
        path = [];
      }
      items = await api.children(folder.id);
      selected = [];
      error = '';
    } catch (e) {
      error = errorText(e);
    }
  }

  async function refresh() {
    if (folder) items = await api.children(folder.id);
    selected = selected.filter((id) => items.some((n) => n.id === id));
  }

  $effect(() => {
    load(folderId, trash);
  });

  /** The trash folder, when this folder is it or is inside it. */
  const trashFolder = $derived.by(() => {
    if (folder && path.length === 1 && folder.name === TRASH) return folder;
    return path.length > 1 && path[1].name === TRASH ? path[1] : null;
  });
  const inTrash = $derived(trashFolder !== null);
  /** Things deleted themselves are here; only they can be restored. */
  const atTrashTop = $derived(inTrash && folder?.id === trashFolder?.id);

  function crumbName(node: FileNode) {
    return node.id === trashFolder?.id ? t.trash : node.name;
  }

  /** Runs an action, shows its error if it fails, and reloads the folder. */
  async function act(action: () => Promise<unknown>) {
    try {
      await action();
      error = '';
    } catch (e) {
      error = errorText(e);
    }
    await refresh();
  }

  /** A folder opens; a file shows its preview, or opens in the first app for it, or downloads. */
  function open(node: FileNode) {
    const opener = openersFor(apps.manifests, node, locale)[0];
    if (node.is_dir) go('/files/' + node.id);
    else if (previewKind(node)) previewing = node;
    else if (opener && !inTrash) openInApp(opener, node);
    else location.href = api.contentUrl(node.id, true);
  }

  // ----- uploads -----

  /** Uploads files and folders into a folder, creating the folders they need. */
  async function uploadEntries(targetId: string, entries: UploadEntry[]) {
    const folders = new FolderMaker(targetId);
    for (const entry of entries) {
      try {
        const folderId = await folders.folderId(entry.folders);
        if (entry.file) startUpload(folderId, entry.file, false, [...entry.folders, entry.file.name].join('/'));
      } catch (e) {
        error = errorText(e);
      }
    }
    if (entries.some((entry) => entry.folders.length) && folder) refresh();
  }

  function startUpload(targetId: string, file: File, overwrite: boolean, label = file.name) {
    uploads.push({ id: nextUploadId++, name: label, progress: 0, state: 'waiting' });
    const item = uploads[uploads.length - 1];

    const run = () => {
      runningUploads++;
      item.state = 'running';
      const upload = uploadFile(targetId, file, overwrite, (fraction) => (item.progress = fraction));
      item.cancel = upload.cancel;
      upload.done
        .then(() => {
          item.state = 'done';
          if (folder?.id === targetId) refresh();
        })
        .catch((e) => {
          if (e.code === 'NAME_TAKEN' && !overwrite && confirm(t.confirmOverwrite(label))) {
            uploads.splice(uploads.indexOf(item), 1);
            startUpload(targetId, file, true, label);
            return;
          }
          item.state = 'failed';
          item.error = errorText(e);
        })
        .finally(() => {
          runningUploads--;
          waitingUploads.shift()?.();
        });
    };
    if (runningUploads < PARALLEL_UPLOADS) run();
    else waitingUploads.push(run);
  }

  function onDrop(event: DragEvent) {
    event.preventDefault();
    dragging = false;
    // Rows dropped on the folder they are already in: nothing to do. Nothing is uploaded to the trash.
    if (!event.dataTransfer || !folder || inTrash || event.dataTransfer.types.includes(NODES_TYPE)) return;
    const targetId = folder.id;
    entriesFromDrop(event.dataTransfer).then((entries) => uploadEntries(targetId, entries));
  }

  // ----- dragging rows onto folders -----

  function onDragNode(node: FileNode, event: DragEvent) {
    if (!event.dataTransfer) return;
    // Dragging a selected row takes the whole selection along.
    const ids = selected.includes(node.id) ? selected : [node.id];
    event.dataTransfer.setData(NODES_TYPE, JSON.stringify(ids));
    event.dataTransfer.effectAllowed = 'copyMove';
  }

  /** A drop on a folder row or a breadcrumb: rows ask "move or copy?", files from the computer are uploaded there. */
  function onDropOnFolder(target: FileNode, event: DragEvent) {
    const data = event.dataTransfer;
    if (!data) return;
    const dragged = data.getData(NODES_TYPE);
    if (dragged) {
      const ids: string[] = JSON.parse(dragged);
      const nodes = items.filter((node) => ids.includes(node.id) && node.id !== target.id);
      if (nodes.length) dropMenu = { x: event.clientX, y: event.clientY, nodes, target };
    } else if (data.types.includes('Files')) {
      entriesFromDrop(data).then((entries) => uploadEntries(target.id, entries));
    }
  }

  /** Drop handlers for a breadcrumb; none for the current folder. */
  function crumbDrop(target: FileNode | undefined) {
    if (!target || target.id === folder?.id) return {};
    return {
      ondragover: (event: DragEvent) => {
        event.preventDefault();
        event.stopPropagation();
      },
      ondrop: (event: DragEvent) => {
        event.preventDefault();
        event.stopPropagation();
        onDropOnFolder(target, event);
      },
    };
  }

  async function onDropChoice(action: 'move' | 'copy' | null) {
    const current = dropMenu;
    dropMenu = null;
    if (action && current) await transfer(action, current.nodes, current.target);
  }

  // ----- new files -----

  /** The "New" menu: text and Markdown, then what the apps add (an office document…). */
  const newFileKinds = $derived([...coreKinds(t), ...appKinds(apps.manifests, locale)]);

  function startNewFile(type: NewFileKind) {
    openNameDialog({ kind: 'newFile', type }, freeName(type.baseName, type.extension, items.map((n) => n.name)));
  }

  /** Creates the file (from the app's template, if it has one) and opens it in the app that opens it. */
  async function createNewFile(parentId: string, type: NewFileKind, name: string) {
    let created: FileNode;
    try {
      const content = type.appId && type.template ? await api.appTemplate(type.appId, type.template) : new Blob([]);
      created = await api.createFile(parentId, name, content);
      error = '';
    } catch (e) {
      error = errorText(e);
      return;
    } finally {
      await refresh();
    }
    const openers = openersFor(apps.manifests, created, locale);
    const opener = openers.find((o) => o.appId === type.appId) ?? openers[0];
    if (opener) openInApp(opener, created);
  }

  /** In the name field of a new file, the name without the extension is selected. */
  function selectBaseName(event: FocusEvent) {
    const input = event.currentTarget as HTMLInputElement;
    const dot = input.value.lastIndexOf('.');
    if (dialog?.kind === 'newFile' && dot > 0) input.setSelectionRange(0, dot);
  }

  // ----- dialogs and actions -----

  function openNameDialog(state: DialogState, initial: string) {
    nameInput = initial;
    dialog = state;
  }

  async function submitName(event: SubmitEvent) {
    event.preventDefault();
    const current = dialog;
    if (!current || !folder) return;
    const parentId = folder.id;
    dialog = null;
    if (current.kind === 'newFolder') await act(() => api.createFolder(parentId, nameInput.trim()));
    if (current.kind === 'newFile') await createNewFile(parentId, current.type, nameInput.trim());
    if (current.kind === 'rename') await act(() => api.update(current.node.id, { name: nameInput.trim() }));
  }

  async function chooseTarget(target: FileNode) {
    const current = dialog;
    dialog = null;
    if (current?.kind !== 'move' && current?.kind !== 'copy') return;
    await transfer(current.kind, current.nodes, target);
  }

  /** Moves or copies nodes into `target`. A copy into the same folder gets a "(copy)" name. */
  async function transfer(kind: 'move' | 'copy', nodes: FileNode[], target: FileNode) {
    await act(async () => {
      for (const node of nodes) {
        if (kind === 'move') {
          if (node.parent_id !== target.id) await api.update(node.id, { parent_id: target.id });
        } else {
          const name = target.id === node.parent_id ? copyName(node.name, node.is_dir) : undefined;
          await api.copy(node.id, target.id, name);
        }
      }
    });
  }

  /** To the trash; in the trash, forever. */
  async function remove(nodes: FileNode[]) {
    if (inTrash) {
      const question = nodes.length === 1 ? t.confirmDelete(nodes[0].name) : t.confirmDeleteMany(nodes.length);
      if (!confirm(question)) return;
    }
    await act(async () => {
      for (const node of nodes) await api.remove(node.id);
    });
  }

  async function restore(nodes: FileNode[]) {
    await act(async () => {
      for (const node of nodes) await api.restore(node.id);
    });
  }

  async function emptyTrash() {
    if (confirm(t.confirmEmptyTrash)) await act(api.emptyTrash);
  }

  const selectedNodes = $derived(items.filter((n) => selected.includes(n.id)));

  function closeMenu(event: Event) {
    (event.currentTarget as HTMLElement).closest('details')?.removeAttribute('open');
  }
</script>

<FilesTabs active={trash || inTrash ? 'trash' : 'files'} />

<section
  class="browser"
  class:dragging
  ondragover={(e) => {
    e.preventDefault();
    // The hint is for files from the computer; dragged rows go to folder rows.
    dragging = !!e.dataTransfer?.types.includes('Files');
  }}
  ondragleave={(e) => {
    if (e.currentTarget === e.target) dragging = false;
  }}
  ondrop={onDrop}
  aria-label={t.files}
>
  <div class="toolbar">
    <nav class="breadcrumbs">
      <a href={href('/files')} {...crumbDrop(path[0] ?? folder ?? undefined)}><Icon name="home" /> {t.home}</a>
      {#each path.slice(1) as part (part.id)}
        <span>/</span><a href={href('/files/' + part.id)} {...crumbDrop(part)}>{crumbName(part)}</a>
      {/each}
      {#if folder?.parent_id}<span>/</span><strong>{crumbName(folder)}</strong>{/if}
    </nav>

    {#if inTrash}
      <div class="buttons">
        {#if selected.length}
          <span class="muted small">{t.selected(selected.length)}</span>
          {#if atTrashTop}
            <button type="button" onclick={() => restore(selectedNodes)}>{t.restore}</button>
          {/if}
          <button type="button" class="danger" onclick={() => remove(selectedNodes)}>
            <Icon name="trash" />
            {t.deleteForever}
          </button>
        {/if}
        {#if atTrashTop && items.length}
          <button type="button" class="danger" onclick={emptyTrash}>{t.emptyTrash}</button>
        {/if}
      </div>
    {:else}
      <div class="buttons">
        {#if selected.length}
          <span class="muted small">{t.selected(selected.length)}</span>
          <button type="button" onclick={() => (dialog = { kind: 'move', nodes: selectedNodes })}>{t.move}</button>
          <button type="button" class="danger" onclick={() => remove(selectedNodes)}>
            <Icon name="trash" />
            {t.deleteSelected}
          </button>
        {/if}
        <details class="menu new-menu">
          <summary class="button"><Icon name="plus" /> {t.newItem}</summary>
          <div class="menu-items">
            <button
              type="button"
              onclick={(e) => {
                closeMenu(e);
                openNameDialog({ kind: 'newFolder' }, '');
              }}
            >
              <Icon name="folder" />
              {t.newFolder}
            </button>
            {#each newFileKinds as type (type.key)}
              <button
                type="button"
                onclick={(e) => {
                  closeMenu(e);
                  startNewFile(type);
                }}
              >
                <Icon name="file" />
                {type.label}
              </button>
            {/each}
          </div>
        </details>
        <button type="button" onclick={() => folderInput?.click()}>
          <Icon name="folder" />
          {t.uploadFolder}
        </button>
        <button type="button" class="primary" onclick={() => fileInput?.click()}>
          <Icon name="upload" />
          {t.upload}
        </button>
        <input
          bind:this={fileInput}
          type="file"
          multiple
          hidden
          onchange={() => {
            if (fileInput?.files && folder) uploadEntries(folder.id, entriesFromInput(fileInput.files));
            if (fileInput) fileInput.value = '';
          }}
        />
        <input
          bind:this={folderInput}
          type="file"
          webkitdirectory
          hidden
          data-testid="folder-input"
          onchange={() => {
            if (folderInput?.files && folder) uploadEntries(folder.id, entriesFromInput(folderInput.files));
            if (folderInput) folderInput.value = '';
          }}
        />
      </div>
    {/if}
  </div>

  {#if error}<p class="error">{error}</p>{/if}
  {#if atTrashTop}<p class="muted small">{t.trashHint}</p>{/if}

  {#if folder}
    {#if inTrash ? items.length : items.some(isShown)}
      <FileList
        nodes={items}
        onopen={open}
        bind:selected
        showAll={inTrash}
        ondragnode={inTrash ? undefined : onDragNode}
        ondropnode={inTrash ? undefined : onDropOnFolder}
      >
        {#snippet actions(node)}
          <details class="menu">
            <summary class="icon" aria-label="⋯"><Icon name="more" /></summary>
            <div class="menu-items">
              {#if !node.is_dir}
                <a href={api.contentUrl(node.id, true)} download onclick={closeMenu}>
                  <Icon name="download" />
                  {t.download}
                </a>
              {:else if !inTrash}
                <a href={api.zipUrl(node.id)} download onclick={closeMenu}>
                  <Icon name="download" />
                  {t.downloadZip}
                </a>
              {/if}
              {#if inTrash}
                {#if atTrashTop}
                  <button
                    type="button"
                    onclick={(e) => {
                      closeMenu(e);
                      restore([node]);
                    }}>{t.restore}</button
                  >
                {/if}
                <button
                  type="button"
                  class="danger"
                  onclick={(e) => {
                    closeMenu(e);
                    remove([node]);
                  }}
                >
                  <Icon name="trash" />
                  {t.deleteForever}
                </button>
              {:else}
                {#each openersFor(apps.manifests, node, locale) as opener (opener.appId + opener.page)}
                  <button
                    type="button"
                    onclick={(e) => {
                      closeMenu(e);
                      openInApp(opener, node);
                    }}
                  >
                    <Icon name="app" />
                    {opener.label ?? t.openIn(opener.appName)}
                  </button>
                {/each}
                <button
                  type="button"
                  onclick={(e) => {
                    closeMenu(e);
                    dialog = { kind: 'share', node };
                  }}
                >
                  <Icon name="link" />
                  {t.share}
                </button>
                {#if node.lock}
                  {@const lock = node.lock}
                  <button
                    type="button"
                    onclick={(e) => {
                      closeMenu(e);
                      const app = apps.manifests.find((a) => a.id === lock.app_id);
                      const name = app ? (app.name[locale] ?? app.name.en) : lock.app_id;
                      if (confirm(t.confirmUnlock(name))) act(() => api.unlock(node.id));
                    }}
                  >
                    <Icon name="lock" />
                    {t.unlock}
                  </button>
                {/if}
                {#if !node.is_dir}
                  <button
                    type="button"
                    onclick={(e) => {
                      closeMenu(e);
                      dialog = { kind: 'versions', node };
                    }}
                  >
                    <Icon name="history" />
                    {t.versions}
                  </button>
                {/if}
                <button
                  type="button"
                  onclick={(e) => {
                    closeMenu(e);
                    openNameDialog({ kind: 'rename', node }, node.name);
                  }}>{t.rename}</button
                >
                <button
                  type="button"
                  onclick={(e) => {
                    closeMenu(e);
                    dialog = { kind: 'move', nodes: [node] };
                  }}>{t.move}</button
                >
                <button
                  type="button"
                  onclick={(e) => {
                    closeMenu(e);
                    dialog = { kind: 'copy', nodes: [node] };
                  }}>{t.copy}</button
                >
                <button
                  type="button"
                  class="danger"
                  onclick={(e) => {
                    closeMenu(e);
                    remove([node]);
                  }}
                >
                  <Icon name="trash" />
                  {t.delete}
                </button>
              {/if}
            </div>
          </details>
        {/snippet}
      </FileList>
    {:else}
      <p class="empty muted">{atTrashTop ? t.trashEmpty : t.emptyFolder}</p>
    {/if}
  {:else if !error}
    <p class="muted">{t.loading}</p>
  {/if}

  {#if dragging}<div class="drop-hint">{t.dropHere}</div>{/if}
</section>

{#if previewing}
  <Preview
    nodes={items}
    start={previewing}
    url={(node) => api.contentUrl(node.id)}
    downloadUrl={(node) => api.contentUrl(node.id, true)}
    onclose={() => (previewing = null)}
  />
{/if}

{#if dropMenu}
  <DropMenu
    x={dropMenu.x}
    y={dropMenu.y}
    count={dropMenu.nodes.length}
    target={dropMenu.target.name || t.home}
    onchoose={onDropChoice}
  />
{/if}

<UploadPanel items={uploads} onclear={() => (uploads = [])} />

{#if dialog?.kind === 'newFolder' || dialog?.kind === 'newFile' || dialog?.kind === 'rename'}
  {@const title =
    dialog.kind === 'newFolder' ? t.newFolder : dialog.kind === 'newFile' ? t.newFileTitle(dialog.type.label) : t.rename}
  <Dialog {title} onclose={() => (dialog = null)}>
    <form class="name-form" onsubmit={submitName}>
      <label>
        {dialog.kind === 'newFolder' ? t.folderName : t.name}
        <!-- svelte-ignore a11y_autofocus -->
        <input bind:value={nameInput} required autofocus onfocus={selectBaseName} />
      </label>
      <footer>
        <button type="button" onclick={() => (dialog = null)}>{t.cancel}</button>
        <button type="submit" class="primary">{dialog.kind === 'rename' ? t.save : t.create}</button>
      </footer>
    </form>
  </Dialog>
{:else if (dialog?.kind === 'move' || dialog?.kind === 'copy') && folder}
  <FolderPicker
    title={dialog.kind === 'move' ? t.moveTo : t.copyTo}
    startId={folder.id}
    exclude={dialog.kind === 'move' ? dialog.nodes.map((n) => n.id) : []}
    onchoose={chooseTarget}
    onclose={() => (dialog = null)}
  />
{:else if dialog?.kind === 'share'}
  <ShareDialog node={dialog.node} onclose={() => (dialog = null)} />
{:else if dialog?.kind === 'versions'}
  <VersionsDialog node={dialog.node} onclose={() => (dialog = null)} onrestored={refresh} />
{/if}

<style>
  .browser {
    position: relative;
    min-height: 60vh;
    border-radius: 12px;
  }
  .browser.dragging {
    outline: 2px dashed var(--accent);
    outline-offset: 4px;
  }
  .toolbar {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 12px;
    flex-wrap: wrap;
    margin-bottom: 12px;
  }
  .breadcrumbs {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
    font-size: 1.05rem;
  }
  .breadcrumbs a {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    color: var(--text);
  }
  .breadcrumbs span {
    color: var(--muted);
  }
  .buttons {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }
  .empty {
    text-align: center;
    padding: 60px 16px;
  }
  .drop-hint {
    position: absolute;
    inset: 0;
    display: grid;
    place-items: center;
    background: color-mix(in srgb, var(--surface) 85%, transparent);
    font-size: 1.2rem;
    color: var(--accent);
    pointer-events: none;
    border-radius: 12px;
  }
  .name-form {
    display: grid;
    gap: 14px;
  }
  /* The "New" menu opens from a button like its neighbours. */
  .new-menu > summary {
    padding: 7px 14px;
  }
  .new-menu > summary:hover {
    background: var(--hover);
  }
</style>
