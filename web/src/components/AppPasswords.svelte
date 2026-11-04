<!-- Settings → App passwords: passwords for CalDAV and other programs, each with its own apps and rights. -->
<script lang="ts">
  import { api, type AppPassword, type AppPasswordAccess } from '../lib/api';
  import { formatDate } from '../lib/format';
  import { errorText, locale, t } from '../lib/i18n';

  let passwords = $state<AppPassword[]>([]);
  /** Apps a password can open: those served under `/app/`. */
  let apps = $state<{ id: string; name: string }[]>([]);
  let error = $state('');

  let name = $state('');
  let access = $state<Record<string, AppPasswordAccess | ''>>({});
  /** The password just created; shown until the user closes it. */
  let created = $state<string | null>(null);
  let copied = $state(false);
  let passwordInput = $state<HTMLInputElement>();

  const appName = (id: string) => apps.find((app) => app.id === id)?.name ?? id;

  async function load() {
    try {
      const [list, all] = await Promise.all([api.appPasswords(), api.apps()]);
      passwords = list;
      apps = all.apps
        .filter((app) => app.ui?.entry.startsWith('/app/'))
        .map((app) => ({ id: app.id, name: app.name[locale] ?? app.name.en ?? app.id }));
      error = '';
    } catch (e) {
      error = errorText(e);
    }
  }

  async function create(event: SubmitEvent) {
    event.preventDefault();
    const scopes = Object.entries(access)
      .filter(([, level]) => level)
      .map(([app, level]) => ({ app, access: level as AppPasswordAccess }));
    if (scopes.length === 0) {
      error = t.appPasswordChooseApp;
      return;
    }
    try {
      const result = await api.createAppPassword(name, scopes);
      const { password, ...info } = result;
      created = password;
      passwords = [info, ...passwords];
      name = '';
      access = {};
      error = '';
    } catch (e) {
      error = errorText(e);
    }
  }

  async function revoke(password: AppPassword) {
    try {
      await api.revokeAppPassword(password.id);
      passwords = passwords.filter((p) => p.id !== password.id);
    } catch (e) {
      error = errorText(e);
    }
  }

  async function copy() {
    if (!created) return;
    try {
      // The clipboard API exists only on HTTPS and localhost.
      await navigator.clipboard.writeText(created);
    } catch {
      passwordInput?.select();
      document.execCommand('copy');
    }
    copied = true;
    setTimeout(() => (copied = false), 1500);
  }

  load();
</script>

<section class="card">
  <h2>{t.appPasswords}</h2>
  <p class="muted small">{t.appPasswordsHint}</p>
  {#if error}<p class="error small">{error}</p>{/if}

  {#if created}
    <div class="created">
      <p class="small">{t.appPasswordCreated}</p>
      <div class="row">
        <input bind:this={passwordInput} readonly value={created} onfocus={() => passwordInput?.select()} />
        <button type="button" onclick={copy}>{copied ? t.appPasswordCopied : t.copy}</button>
      </div>
      <button type="button" onclick={() => (created = null)}>{t.done}</button>
    </div>
  {/if}

  {#if passwords.length > 0}
    <ul>
      {#each passwords as password (password.id)}
        <li>
          <div class="what">
            <span class="name">{password.name}</span>
            <span class="muted small">
              {password.scopes
                .map((s) => `${appName(s.app)}: ${s.access === 'write' ? t.accessWrite : t.accessRead}`)
                .join(', ')}
            </span>
            <span class="muted small">
              {t.accessSince(formatDate(password.created_at, locale))} ·
              {password.last_used_at ? t.appPasswordUsed(formatDate(password.last_used_at, locale)) : t.appPasswordNeverUsed}
            </span>
          </div>
          <button type="button" onclick={() => revoke(password)}>{t.revoke}</button>
        </li>
      {/each}
    </ul>
  {/if}

  {#if apps.length > 0}
    <form onsubmit={create}>
      <h3>{t.appPasswordNew}</h3>
      <label>{t.appPasswordName}<input bind:value={name} placeholder={t.appPasswordNamePlaceholder} required /></label>
      {#each apps as app (app.id)}
        <label class="scope">
          <span>{app.name}</span>
          <select bind:value={access[app.id]}>
            <option value="">{t.appPasswordNoAccess}</option>
            <option value="read">{t.accessRead}</option>
            <option value="write">{t.accessWrite}</option>
          </select>
        </label>
      {/each}
      <button type="submit" class="primary">{t.create}</button>
    </form>
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
  form {
    display: grid;
    gap: 8px;
  }
  form button {
    justify-self: start;
  }
  .scope {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
  }
  .created {
    display: grid;
    gap: 6px;
    margin-bottom: 12px;
  }
  .created button:last-child {
    justify-self: start;
  }
  .row {
    display: flex;
    gap: 6px;
  }
  .row input {
    flex: 1;
    min-width: 0;
    font-family: var(--mono);
  }
</style>
