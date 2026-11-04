<!-- Dialog for choosing a target folder (move / copy). -->
<script lang="ts">
  import { api, type FileNode } from '../lib/api';
  import { errorText, t } from '../lib/i18n';
  import Dialog from './Dialog.svelte';
  import Icon from './Icon.svelte';

  let {
    title,
    startId,
    exclude,
    onchoose,
    onclose,
  }: {
    title: string;
    startId: string;
    /** Folders that may not be chosen or entered (the ones being moved). */
    exclude: string[];
    onchoose: (folder: FileNode) => void;
    onclose: () => void;
  } = $props();

  let current = $state<FileNode | null>(null);
  let folders = $state<FileNode[]>([]);
  let error = $state('');

  async function open(id: string) {
    try {
      const details = await api.details(id);
      current = details.node;
      folders = (await api.children(id)).filter((n) => n.is_dir && !exclude.includes(n.id));
      error = '';
    } catch (e) {
      error = errorText(e);
    }
  }

  $effect(() => {
    open(startId);
  });
</script>

<Dialog {title} {onclose}>
  {#if current}
    <div class="where">
      <button type="button" disabled={!current.parent_id} onclick={() => current?.parent_id && open(current.parent_id)}>
        <Icon name="up" />
        {t.up}
      </button>
      <strong>{current.parent_id ? current.name : t.home}</strong>
    </div>
    <ul>
      {#each folders as folder (folder.id)}
        <li>
          <button type="button" class="link" onclick={() => open(folder.id)}>
            <Icon name="folder" />
            {folder.name}
          </button>
        </li>
      {/each}
    </ul>
  {/if}
  {#if error}<p class="error">{error}</p>{/if}
  <footer>
    <button type="button" onclick={onclose}>{t.cancel}</button>
    <button type="button" class="primary" disabled={!current} onclick={() => current && onchoose(current)}>
      {t.chooseFolder}
    </button>
  </footer>
</Dialog>

<style>
  .where {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-bottom: 8px;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    max-height: 300px;
    overflow: auto;
    border: 1px solid var(--border);
    border-radius: 8px;
    min-height: 120px;
  }
  li .link {
    display: flex;
    gap: 8px;
    align-items: center;
    width: 100%;
    padding: 8px 10px;
  }
  li .link:hover {
    background: var(--hover);
  }
</style>
