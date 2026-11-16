<!-- All the user shared: with people, and public links; what a plugin made says so. -->
<script lang="ts">
  import { api, type Access, type FileNode, type Share } from '../lib/api';
  import { appName } from '../lib/apps.svelte';
  import { formatDate } from '../lib/format';
  import { errorText, locale, t } from '../lib/i18n';
  import { href } from '../lib/router.svelte';
  import FilesTabs from '../components/FilesTabs.svelte';
  import Icon from '../components/Icon.svelte';
  import ShareLink from '../components/ShareLink.svelte';

  let links = $state<Share[] | null>(null);
  let given = $state<(Access & { node: FileNode })[]>([]);
  let error = $state('');

  async function refresh() {
    try {
      [links, given] = await Promise.all([api.shares(), api.givenAccess()]);
    } catch (e) {
      error = errorText(e);
    }
  }

  async function stop(access: Access) {
    await api.removeAccess(access.id);
    await refresh();
  }

  async function remove(link: Share) {
    await api.deleteShare(link.id);
    await refresh();
  }

  /** Where the shared item can be seen in the file browser. */
  function folderOf(link: { node: FileNode }): string {
    return href('/files/' + (link.node.is_dir ? link.node.id : link.node.parent_id));
  }

  refresh();
</script>

<FilesTabs active="links" />
{#if error}<p class="error">{error}</p>{/if}
{#if links === null}
  <p class="muted">{t.loading}</p>
{:else if links.length === 0 && given.length === 0}
  <p class="muted">{t.noLinks}</p>
{:else}
  {#if given.length}
    <h3>{t.givenAccessTitle}</h3>
    <div class="links">
      {#each given as access (access.id)}
        <article class="card">
          <a class="item" href={folderOf(access)}>
            <Icon name={access.node.is_dir ? 'folder' : 'file'} />
            {access.node.name}
          </a>
          <div class="meta">
            <span class="muted small">
              {access.user ? access.user.username : access.group?.name}{access.write ? '' : ` · ${t.readOnlyShare}`}
              {access.app ? ` · ${t.viaApp(appName(access.app))}` : ''} · {formatDate(access.created_at, locale)}
            </span>
            <button type="button" class="danger small" onclick={() => stop(access)}>{t.stopSharing}</button>
          </div>
        </article>
      {/each}
    </div>
  {/if}
  {#if links.length}<h3>{t.publicLinksTitle}</h3>{/if}
  <div class="links">
    {#each links as link (link.id)}
      <article class="card">
        <a class="item" href={folderOf(link)}>
          <Icon name={link.node.is_dir ? 'folder' : 'file'} />
          {link.node.name}
        </a>
        <ShareLink token={link.token} />
        <div class="meta">
          <span class="muted small">
            {t.created}: {formatDate(link.created_at, locale)} ·
            {t.linkExpires}: {link.expires_at ? formatDate(link.expires_at, locale) : t.never}
            {link.app ? ` · ${t.viaApp(appName(link.app))}` : ''}
          </span>
          <button type="button" class="danger small" onclick={() => remove(link)}>{t.deleteLink}</button>
        </div>
      </article>
    {/each}
  </div>
{/if}

<style>
  .links {
    display: grid;
    gap: 12px;
  }
  .card {
    display: grid;
    gap: 8px;
  }
  .item {
    display: flex;
    align-items: center;
    gap: 8px;
    font-weight: 500;
    color: var(--text);
  }
  .meta {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }
</style>
