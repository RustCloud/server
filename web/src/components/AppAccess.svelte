<!-- Settings → App access: what each app may do with the user's files, with revoke. -->
<script lang="ts">
  import { api, type Grant } from '../lib/api';
  import { formatDate } from '../lib/format';
  import { errorText, locale, t } from '../lib/i18n';
  import Icon from './Icon.svelte';

  let grants = $state<Grant[]>([]);
  let names = $state<Record<string, string>>({});
  let error = $state('');

  /** Grants grouped by app, apps in the order of their first grant. */
  const byApp = $derived.by(() => {
    const groups = new Map<string, Grant[]>();
    for (const grant of grants) groups.set(grant.app_id, [...(groups.get(grant.app_id) ?? []), grant]);
    return [...groups];
  });

  async function load() {
    try {
      const [list, apps] = await Promise.all([api.grants(), api.apps()]);
      grants = list;
      names = Object.fromEntries(apps.apps.map((app) => [app.id, app.name[locale] ?? app.name.en ?? app.id]));
      error = '';
    } catch (e) {
      error = errorText(e);
    }
  }

  async function revoke(grant: Grant) {
    try {
      await api.revokeGrant(grant.id);
      grants = grants.filter((g) => g.id !== grant.id);
    } catch (e) {
      error = errorText(e);
    }
  }

  load();
</script>

<section class="card">
  <h2>{t.appAccess}</h2>
  {#if error}<p class="error small">{error}</p>{/if}
  {#each byApp as [appId, list] (appId)}
    <h3>{names[appId] ?? appId}</h3>
    <ul>
      {#each list as grant (grant.id)}
        <li>
          <Icon name={grant.node.is_dir ? 'folder' : 'file'} />
          <div class="what">
            <span class="name">{grant.node.parent_id === null ? '/' : grant.node.name}</span>
            <span class="muted small">
              {grant.write ? t.accessWrite : t.accessRead} ·
              {grant.kind === 'session' ? t.accessWhileOpen : t.accessStanding} ·
              {t.accessSince(formatDate(grant.created_at, locale))}
            </span>
          </div>
          <button type="button" onclick={() => revoke(grant)}>{t.revoke}</button>
        </li>
      {/each}
    </ul>
  {:else}
    {#if !error}<p class="muted">{t.appAccessNone}</p>{/if}
  {/each}
</section>

<style>
  h2 {
    font-size: 1.05rem;
    margin: 0 0 8px;
  }
  h3 {
    font-size: 0.95rem;
    font-weight: 500;
    margin: 12px 0 4px;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  li {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 6px 0;
    border-bottom: 1px solid var(--border-soft);
  }
  .what {
    flex: 1;
    min-width: 0;
    display: grid;
  }
  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
