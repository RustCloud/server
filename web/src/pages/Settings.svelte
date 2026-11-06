<!-- The user's own profile, password and ways to log in, storage usage, the access given to apps, shared app data and app passwords. -->
<script lang="ts">
  import AppAccess from '../components/AppAccess.svelte';
  import AppPasswords from '../components/AppPasswords.svelte';
  import AppShares from '../components/AppShares.svelte';
  import LoginMethods from '../components/LoginMethods.svelte';
  import { api, type Usage } from '../lib/api';
  import { formatBytes } from '../lib/format';
  import { errorText, t } from '../lib/i18n';
  import { prefs, setShowHidden } from '../lib/prefs.svelte';
  import { session } from '../lib/session.svelte';

  let displayName = $state(session.user?.display_name ?? '');
  let email = $state(session.user?.email ?? '');
  let profileMessage = $state('');

  let currentPassword = $state('');
  let newPassword = $state('');
  let passwordMessage = $state('');
  let passwordError = $state(false);
  let revokeAppPasswords = $state(false);
  let sessionsMessage = $state('');
  /** Accounts of login services may have no password to change. */
  let hasPassword = $state(true);

  let usage = $state<Usage | null>(null);
  api.usage().then((u) => (usage = u));

  async function saveProfile(event: SubmitEvent) {
    event.preventDefault();
    try {
      session.user = await api.updateProfile({ display_name: displayName, email });
      profileMessage = t.saved;
    } catch (e) {
      profileMessage = errorText(e);
    }
  }

  async function savePassword(event: SubmitEvent) {
    event.preventDefault();
    try {
      await api.changePassword(currentPassword, newPassword, revokeAppPasswords);
      currentPassword = newPassword = '';
      passwordError = false;
      passwordMessage = revokeAppPasswords ? t.passwordChangedAppPasswordsRevoked : t.passwordChanged;
      revokeAppPasswords = false;
    } catch (e) {
      passwordError = true;
      passwordMessage = errorText(e);
    }
  }

  async function logoutOthers() {
    try {
      const { ended } = await api.logoutOthers();
      sessionsMessage = t.otherSessionsEnded(ended);
    } catch (e) {
      sessionsMessage = errorText(e);
    }
  }
</script>

<h1>{t.settings}</h1>

<div class="grid">
  <form class="card" onsubmit={saveProfile}>
    <h2>{t.profile}</h2>
    <label>{t.username}<input value={session.user?.username} disabled /></label>
    <label>{t.displayName}<input bind:value={displayName} /></label>
    <label>{t.email}<input type="email" bind:value={email} /></label>
    <button type="submit" class="primary">{t.save}</button>
    {#if profileMessage}<p class="muted small">{profileMessage}</p>{/if}
  </form>

  {#if hasPassword}<form class="card" onsubmit={savePassword}>
    <h2>{t.changePassword}</h2>
    <label>
      {t.currentPassword}
      <input type="password" bind:value={currentPassword} autocomplete="current-password" required />
    </label>
    <label>
      {t.newPassword}
      <input type="password" bind:value={newPassword} autocomplete="new-password" minlength="8" required />
    </label>
    <label class="check">
      <input type="checkbox" bind:checked={revokeAppPasswords} />
      {t.revokeAppPasswords}
    </label>
    <button type="submit" class="primary">{t.save}</button>
    {#if passwordMessage}<p class:error={passwordError} class="small">{passwordMessage}</p>{/if}
  </form>{/if}

  <LoginMethods bind:hasPassword />

  <section class="card">
    <h2>{t.sessions}</h2>
    <p class="muted small">{t.sessionsHint}</p>
    <button type="button" onclick={logoutOthers}>{t.logoutOthers}</button>
    {#if sessionsMessage}<p class="small">{sessionsMessage}</p>{/if}
  </section>

  {#if usage}
    <section class="card">
      <h2>{t.storage}</h2>
      {#if usage.quota_bytes !== null}
        <progress max={usage.quota_bytes} value={usage.used_bytes}></progress>
        <p>{t.usedOf(formatBytes(usage.used_bytes), formatBytes(usage.quota_bytes))}</p>
      {:else}
        <p>{t.usedNoLimit(formatBytes(usage.used_bytes))}</p>
      {/if}
      {#if usage.versions_bytes > 0}<p class="muted small">{t.versionsUse(formatBytes(usage.versions_bytes))}</p>{/if}
    </section>
  {/if}

  <section class="card">
    <h2>{t.display}</h2>
    <label class="check">
      <input type="checkbox" checked={prefs.showHidden} onchange={(e) => setShowHidden(e.currentTarget.checked)} />
      {t.showHidden}
    </label>
    <p class="muted small">{t.showHiddenHint}</p>
  </section>

  <AppAccess />
  <AppShares />
  <AppPasswords />
</div>

<style>
  .grid {
    display: grid;
    gap: 16px;
    grid-template-columns: repeat(auto-fit, minmax(280px, 1fr));
    align-items: start;
  }
  form {
    display: grid;
    gap: 12px;
    justify-items: stretch;
  }
  form button {
    justify-self: start;
  }
  h2 {
    font-size: 1.05rem;
    margin: 0;
  }
  progress {
    width: 100%;
    height: 10px;
    accent-color: var(--accent);
  }
  .check {
    display: flex;
    align-items: center;
    gap: 8px;
  }
</style>
