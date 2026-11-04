<!-- Settings → Shared app data: what the user shared from apps (a calendar) and what others shared, with revoke. -->
<script lang="ts">
  import { api, type AppShare } from '../lib/api';
  import { formatDate } from '../lib/format';
  import { errorText, locale, t } from '../lib/i18n';

  let given = $state<AppShare[]>([]);
  let received = $state<AppShare[]>([]);
  let names = $state<Record<string, string>>({});
  let error = $state('');

  const appName = (id: string) => names[id] ?? id;
  const level = (share: AppShare) => (share.write ? t.appShareCanWrite : t.appShareCanRead);

  async function load() {
    try {
      const [shares, apps] = await Promise.all([api.appShares(), api.apps()]);
      given = shares.given;
      received = shares.received;
      names = Object.fromEntries(apps.apps.map((app) => [app.id, app.name[locale] ?? app.name.en ?? app.id]));
      error = '';
    } catch (e) {
      error = errorText(e);
    }
  }

  async function revoke(share: AppShare) {
    try {
      await api.revokeAppShare(share.id);
      given = given.filter((s) => s.id !== share.id);
      received = received.filter((s) => s.id !== share.id);
    } catch (e) {
      error = errorText(e);
    }
  }

  load();
</script>

<section class="card">
  <h2>{t.appShares}</h2>
  {#if error}<p class="error small">{error}</p>{/if}
  {#if given.length > 0}
    <h3>{t.appSharesGiven}</h3>
    <ul>
      {#each given as share (share.id)}
        <li>
          <div class="what">
            <span class="name">{t.appShareWith(share.label, share.user.display_name)}</span>
            <span class="muted small">
              {appName(share.app_id)} · {level(share)} · {t.accessSince(formatDate(share.created_at, locale))}
            </span>
          </div>
          <button type="button" onclick={() => revoke(share)}>{t.revoke}</button>
        </li>
      {/each}
    </ul>
  {/if}
  {#if received.length > 0}
    <h3>{t.appSharesReceived}</h3>
    <ul>
      {#each received as share (share.id)}
        <li>
          <div class="what">
            <span class="name">{t.appShareFrom(share.label, share.owner.display_name)}</span>
            <span class="muted small">
              {appName(share.app_id)} · {level(share)} · {t.accessSince(formatDate(share.created_at, locale))}
            </span>
          </div>
          <button type="button" onclick={() => revoke(share)}>{t.appShareLeave}</button>
        </li>
      {/each}
    </ul>
  {/if}
  {#if !error && given.length === 0 && received.length === 0}
    <p class="muted">{t.appSharesNone}</p>
  {/if}
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
