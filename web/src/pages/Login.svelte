<!-- Login, and registration when it is allowed (always for the very first account). -->
<script lang="ts">
  import { api, type AuthStatus } from '../lib/api';
  import { errorText, t } from '../lib/i18n';
  import { session } from '../lib/session.svelte';
  import Logo from '../components/Logo.svelte';

  let status = $state<AuthStatus | null>(null);
  let registering = $state(false);
  let username = $state('');
  let password = $state('');
  let displayName = $state('');
  let error = $state('');
  let busy = $state(false);

  api.status().then((s) => {
    status = s;
    registering = s.needs_setup;
  });

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    busy = true;
    error = '';
    try {
      const result = registering
        ? await api.register(username, password, displayName || undefined)
        : await api.login(username, password);
      session.user = result.user;
    } catch (e) {
      error = errorText(e);
    } finally {
      busy = false;
    }
  }
</script>

<div class="login">
  <form class="card" onsubmit={submit}>
    <div class="brand"><Logo size={32} /> {t.appName}</div>
    {#if status?.needs_setup}
      <h1>{t.setupTitle}</h1>
      <p class="muted">{t.setupHint}</p>
    {/if}

    <label>
      {t.username}
      <input bind:value={username} autocomplete="username" required />
    </label>
    {#if registering}
      <label>
        {t.displayName}
        <input bind:value={displayName} autocomplete="name" />
      </label>
    {/if}
    <label>
      {t.password}
      <input
        type="password"
        bind:value={password}
        autocomplete={registering ? 'new-password' : 'current-password'}
        minlength={registering ? 8 : undefined}
        required
      />
    </label>

    {#if error}<p class="error">{error}</p>{/if}
    <button type="submit" class="primary" disabled={busy}>{registering ? t.createAccount : t.login}</button>

    {#if status?.registration_open && !status.needs_setup}
      <p class="muted small switch">
        {registering ? t.haveAccount : t.noAccount}
        <button type="button" class="link" onclick={() => (registering = !registering)}>
          {registering ? t.login : t.createAccount}
        </button>
      </p>
    {/if}
  </form>
</div>

<style>
  .login {
    min-height: 100vh;
    display: grid;
    place-items: center;
    padding: 16px;
  }
  .card {
    width: min(380px, 100%);
    display: grid;
    gap: 14px;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: 1.4rem;
    font-weight: 600;
    color: var(--accent);
  }
  h1 {
    font-size: 1.2rem;
    margin: 0;
  }
  .switch .link {
    color: var(--accent);
  }
</style>
