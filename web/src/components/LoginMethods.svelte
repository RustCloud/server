<!--
  Settings → Ways to log in: the password and the login services linked to
  the account, with buttons to add a service or remove one. Shown only when
  the server has login services or the account uses one.
-->
<script lang="ts">
  import { api, type LinkedLogin, type OidcButton } from '../lib/api';
  import { formatDate } from '../lib/format';
  import { errorText, locale, t } from '../lib/i18n';
  import { go, route } from '../lib/router.svelte';

  /** Whether the account has a password (the page hides the password form otherwise). */
  let { hasPassword = $bindable(true) }: { hasPassword?: boolean } = $props();

  let logins = $state<LinkedLogin[]>([]);
  let services = $state<OidcButton[]>([]);
  let message = $state('');
  let error = $state('');
  let busy = $state(false);

  const PREFIX = 'oidc:';
  const serviceName = (provider: string) => {
    const id = provider.slice(PREFIX.length);
    return services.find((s) => s.id === id)?.name ?? id;
  };
  const linked = $derived(new Set(logins.map((l) => l.provider)));
  const toAdd = $derived(services.filter((s) => !linked.has(PREFIX + s.id)));
  const external = $derived(logins.filter((l) => l.provider.startsWith(PREFIX)));

  async function load() {
    try {
      [logins, services] = await Promise.all([api.linkedLogins(), api.oidcButtons()]);
      hasPassword = logins.some((l) => l.provider === 'password');
    } catch (e) {
      error = errorText(e);
    }
  }

  async function add(service: OidcButton) {
    busy = true;
    error = '';
    try {
      const { url } = await api.oidcLink(service.id);
      location.href = url;
    } catch (e) {
      error = errorText(e);
      busy = false;
    }
  }

  async function remove(login: LinkedLogin) {
    error = message = '';
    try {
      await api.unlinkLogin(login.id);
      await load();
    } catch (e) {
      error = errorText(e);
    }
  }

  // The service sends the browser back here after adding it, or with an error.
  const added = route.query.get('oidc_linked');
  const failed = route.query.get('oidc_error');
  if (added || failed) {
    go('/settings');
  }

  load().then(() => {
    if (added) message = t.loginLinked(serviceName(PREFIX + added));
    if (failed) error = t.errors[failed] ?? t.errors.OIDC_FAILED;
  });
</script>

{#if services.length || external.length}
  <section class="card">
    <h2>{t.loginMethods}</h2>
    <p class="muted small">{hasPassword ? t.loginMethodsHint : t.noPasswordHint}</p>
    <ul>
      {#each logins as login (login.id)}
        <li>
          <span>
            {login.provider === 'password' ? t.password : serviceName(login.provider)}
            <span class="muted small">{t.loginAdded(formatDate(login.created_at, locale))}</span>
          </span>
          {#if login.provider.startsWith(PREFIX)}
            <button type="button" class="small" disabled={logins.length < 2} onclick={() => remove(login)}>
              {t.unlink}
            </button>
          {/if}
        </li>
      {/each}
    </ul>
    {#each toAdd as service (service.id)}
      <button type="button" disabled={busy} onclick={() => add(service)}>{t.linkService(service.name)}</button>
    {/each}
    {#if message}<p class="note small" role="status">{message}</p>{/if}
    {#if error}<p class="error small" role="alert">{error}</p>{/if}
  </section>
{/if}

<style>
  h2 {
    font-size: 1.05rem;
    margin: 0;
  }
  section {
    display: grid;
    gap: 10px;
    justify-items: start;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    width: 100%;
    display: grid;
    gap: 6px;
  }
  li {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 8px;
  }
  p {
    margin: 0;
  }
</style>
