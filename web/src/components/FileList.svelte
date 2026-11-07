<!-- A table of files and folders, used by the file browser, search and public links. -->
<script lang="ts">
  import type { Snippet } from 'svelte';
  import type { FileNode } from '../lib/api';
  import { apps } from '../lib/apps.svelte';
  import { formatBytes, formatDate } from '../lib/format';
  import { locale, t } from '../lib/i18n';
  import { isHidden, isShown } from '../lib/prefs.svelte';
  import Icon from './Icon.svelte';

  let {
    nodes,
    onopen,
    selected = $bindable(),
    actions,
    ondragnode,
    ondropnode,
    showAll = false,
  }: {
    nodes: FileNode[];
    /** Called when a name is clicked. */
    onopen: (node: FileNode) => void;
    /** Ids of checked rows; selection checkboxes are shown only when bound. */
    selected?: string[];
    /** Buttons at the end of each row. */
    actions?: Snippet<[FileNode]>;
    /** Makes rows draggable; called when dragging a row starts. */
    ondragnode?: (node: FileNode, event: DragEvent) => void;
    /** Makes folder rows drop targets; called on a drop. */
    ondropnode?: (folder: FileNode, event: DragEvent) => void;
    /** Hidden files too, whatever the preference (the trash shows all it has). */
    showAll?: boolean;
  } = $props();

  /** Hidden files (name starting with a dot) only when the user shows them. */
  const shown = $derived(showAll ? nodes : nodes.filter(isShown));

  /** The folder row something is being dragged over. */
  let dropTarget = $state<string | null>(null);

  function dropHandlers(node: FileNode) {
    if (!ondropnode || !node.is_dir) return {};
    return {
      ondragover: (event: DragEvent) => {
        event.preventDefault();
        event.stopPropagation();
        dropTarget = node.id;
      },
      ondragleave: (event: DragEvent) => {
        const row = event.currentTarget as HTMLElement;
        if (!row.contains(event.relatedTarget as Node | null)) dropTarget = null;
      },
      ondrop: (event: DragEvent) => {
        event.preventDefault();
        event.stopPropagation();
        dropTarget = null;
        ondropnode(node, event);
      },
    };
  }

  const allSelected = $derived(selected !== undefined && nodes.length > 0 && selected.length === nodes.length);

  /** An app's name, for the lock it holds. */
  function appName(id: string): string {
    const app = apps.manifests.find((a) => a.id === id);
    return app ? (app.name[locale] ?? app.name.en ?? id) : id;
  }

  function toggle(id: string) {
    if (!selected) return;
    selected = selected.includes(id) ? selected.filter((s) => s !== id) : [...selected, id];
  }

  function toggleAll() {
    selected = allSelected ? [] : nodes.map((n) => n.id);
  }

  function iconFor(node: FileNode) {
    if (node.is_dir) return 'folder' as const;
    return node.mime_type?.startsWith('image/') ? ('image' as const) : ('file' as const);
  }
</script>

<table class="files">
  <thead>
    <tr>
      {#if selected}
        <th class="check"><input type="checkbox" checked={allSelected} onchange={toggleAll} aria-label="all" /></th>
      {/if}
      <th>{t.name}</th>
      <th class="size">{t.size}</th>
      <th class="date">{t.modified}</th>
      {#if actions}<th class="actions"></th>{/if}
    </tr>
  </thead>
  <tbody>
    {#each shown as node (node.id)}
      <tr
        class:hidden-file={isHidden(node)}
        class:checked={selected?.includes(node.id)}
        class:drop-target={dropTarget === node.id}
        draggable={ondragnode ? 'true' : undefined}
        ondragstart={ondragnode && ((event) => ondragnode(node, event))}
        {...dropHandlers(node)}
      >
        {#if selected}
          <td class="check">
            <input
              type="checkbox"
              checked={selected.includes(node.id)}
              onchange={() => toggle(node.id)}
              aria-label={node.name}
            />
          </td>
        {/if}
        <td class="name">
          <button type="button" class="link" onclick={() => onopen(node)}>
            <span class:folder={node.is_dir}><Icon name={iconFor(node)} /></span>
            <span class="label">{node.name}</span>
            {#if node.lock}
              <span class="lock muted" title={t.lockedBy(appName(node.lock.app_id))}><Icon name="lock" size={14} /></span>
            {/if}
          </button>
        </td>
        <td class="size">{node.is_dir ? '' : formatBytes(node.size)}</td>
        <td class="date">{formatDate(node.modified_at, locale)}</td>
        {#if actions}<td class="actions">{@render actions(node)}</td>{/if}
      </tr>
    {/each}
  </tbody>
</table>

<style>
  .lock {
    display: inline-flex;
    margin-left: 4px;
  }
  .files {
    width: 100%;
    border-collapse: collapse;
  }
  th {
    text-align: left;
    font-weight: 500;
    color: var(--muted);
    font-size: 0.85rem;
    padding: 8px;
    border-bottom: 1px solid var(--border);
  }
  td {
    padding: 4px 8px;
    border-bottom: 1px solid var(--border-soft);
  }
  tr.checked td {
    background: var(--selected);
  }
  tr.drop-target td {
    background: var(--selected);
    box-shadow: inset 0 2px 0 var(--accent), inset 0 -2px 0 var(--accent);
  }
  .check {
    width: 28px;
  }
  .name .link {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 6px 0;
    text-align: left;
    width: 100%;
  }
  .label {
    overflow-wrap: anywhere;
  }
  .folder {
    color: var(--accent);
  }
  .size,
  .date {
    color: var(--muted);
    white-space: nowrap;
    font-size: 0.9rem;
  }
  .size {
    width: 90px;
  }
  .date {
    width: 170px;
  }
  .actions {
    width: 1%;
    white-space: nowrap;
    text-align: right;
  }
  @media (max-width: 640px) {
    .date {
      display: none;
    }
  }
  .hidden-file {
    opacity: 0.55;
  }
</style>
