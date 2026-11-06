<!--
  Administration → Accounts → Login through other services: the OpenID
  Connect services users may log in with. Adding or editing one opens a
  dialog; the server checks the issuer before it adds a service.
-->
<script lang="ts">
  import { api, type OidcProvider, type OidcProviderInput } from '../lib/api';
  import { errorText, t } from '../lib/i18n';
  import { server } from '../lib/server.svelte';
  import Dialog from './Dialog.svelte';

  let providers = $state<OidcProvider[]>([]);
  let error = $state('');

  /** The service in the dialog: `null` closed, `'new'` a new one. */
  let editing = $state<OidcProvider | 'new' | null>(null);
  let form = $state<OidcProviderInput & { id: string; issuer: string; client_secret: string }>(blank());
  let userGroups = $state('');
  let adminGroups = $state('');
  let saving = $state(false);
  let formError = $state('');

  function blank() {
    return {
      id: '',
      name: '',
      issuer: '',
      client_id: '',
      client_secret: '',
      scopes: 'openid profile email',
      enabled: true,
      auto_create: true,
      link_by_email: false,
      groups_claim: 'groups',
      user_groups: [],
      admin_groups: [],
    };
  }

  const lines = (text: string) =>
    text
      .split('\n')
      .map((line) => line.trim())
      .filter(Boolean);
  const label = (key: string) => t.oidcFields[key]?.[0] ?? key;
  const hint = (key: string) => t.oidcFields[key]?.[1] ?? '';
  /** The redirect URI of the service in the dialog. */
  const redirectUrl = $derived(
    editing && editing !== 'new'
      ? editing.redirect_url
      : `${server.publicUrl}/api/v1/auth/oidc/${form.id.trim() || '…'}/callback`,
  );

  async function load() {
    try {
      providers = await api.oidcProviders();
      error = '';
    } catch (e) {
      error = errorText(e);
    }
  }

  function open(provider: OidcProvider | 'new') {
    editing = provider;
    formError = '';
    form = provider === 'new' ? blank() : { ...provider, client_secret: '' };
    userGroups = (provider === 'new' ? [] : provider.user_groups).join('\n');
    adminGroups = (provider === 'new' ? [] : provider.admin_groups).join('\n');
  }

  async function save(event: SubmitEvent) {
    event.preventDefault();
    if (!editing) return;
    saving = true;
    formError = '';
    const input = { ...form, user_groups: lines(userGroups), admin_groups: lines(adminGroups) };
    try {
      if (editing === 'new') await api.addOidcProvider(input);
      else await api.changeOidcProvider(editing.id, input);
      editing = null;
      await load();
    } catch (e) {
      formError = errorText(e);
    } finally {
      saving = false;
    }
  }

  async function remove(provider: OidcProvider) {
    if (!confirm(t.oidcConfirmRemove(provider.name))) return;
    try {
      await api.removeOidcProvider(provider.id);
      await load();
    } catch (e) {
      error = errorText(e);
    }
  }

  load();
</script>

<section class="card">
  <div class="head">
    <h2>{t.oidcTitle}</h2>
    <button type="button" onclick={() => open('new')}>{t.oidcAdd}</button>
  </div>
  <p class="muted small">{t.oidcHint}</p>
  {#if error}<p class="error small" role="alert">{error}</p>{/if}
  {#if providers.length === 0}
    <p class="muted small">{t.oidcEmpty}</p>
  {:else}
    <ul>
      {#each providers as provider (provider.id)}
        <li>
          <div class="about">
            <strong>{provider.name}</strong>
            {#if !provider.enabled}<span class="badge">{t.oidcOff}</span>{/if}
            <span class="muted small">{provider.id} · {provider.issuer}</span>
          </div>
          <button type="button" class="small" onclick={() => open(provider)}>{t.oidcEdit}</button>
          <button type="button" class="small" onclick={() => remove(provider)}>{t.delete}</button>
        </li>
      {/each}
    </ul>
  {/if}
</section>

{#if editing}
  <Dialog title={editing === 'new' ? t.oidcAdd : editing.name} onclose={() => (editing = null)} wide>
    <form onsubmit={save}>
      <div class="two">
        <label>
          {label('name')}
          <input bind:value={form.name} required maxlength="60" />
          <span class="muted small">{hint('name')}</span>
        </label>
        <label>
          {label('id')}
          <input
            bind:value={form.id}
            required
            pattern={'[a-z0-9][a-z0-9\\-]{0,31}'}
            disabled={editing !== 'new'}
            autocomplete="off"
          />
          <span class="muted small">{hint('id')}</span>
        </label>
      </div>
      <label>
        {label('issuer')}
        <input type="url" bind:value={form.issuer} required disabled={editing !== 'new'} autocomplete="off" />
        <span class="muted small">{hint('issuer')}</span>
      </label>
      <label>
        {label('redirect_url')}
        <input readonly value={redirectUrl} class="mono" onfocus={(e) => e.currentTarget.select()} />
        <span class="muted small">{hint('redirect_url')}</span>
      </label>
      <div class="two">
        <label>
          {label('client_id')}
          <input bind:value={form.client_id} required autocomplete="off" />
        </label>
        <label>
          {label('client_secret')}
          <input
            type="password"
            bind:value={form.client_secret}
            autocomplete="new-password"
            placeholder={editing !== 'new' && editing.has_secret ? '••••••••' : ''}
          />
          <span class="muted small">
            {editing !== 'new' && editing.has_secret ? t.oidcSecretKept : hint('client_secret')}
          </span>
        </label>
      </div>
      <div class="two">
        <label>
          {label('scopes')}
          <input bind:value={form.scopes} />
          <span class="muted small">{hint('scopes')}</span>
        </label>
        <label>
          {label('groups_claim')}
          <input bind:value={form.groups_claim} />
          <span class="muted small">{hint('groups_claim')}</span>
        </label>
      </div>
      <div class="two">
        <label>
          {label('user_groups')}
          <textarea rows="3" bind:value={userGroups}></textarea>
          <span class="muted small">{hint('user_groups')}</span>
        </label>
        <label>
          {label('admin_groups')}
          <textarea rows="3" bind:value={adminGroups}></textarea>
          <span class="muted small">{hint('admin_groups')}</span>
        </label>
      </div>
      {#each ['enabled', 'auto_create', 'link_by_email'] as const as key (key)}
        <label class="check">
          <input type="checkbox" bind:checked={form[key]} />
          <span>
            {label(key)}
            {#if hint(key)}<span class="muted small hint">{hint(key)}</span>{/if}
          </span>
        </label>
      {/each}
      {#if formError}<p class="error small" role="alert">{formError}</p>{/if}
      <div class="buttons">
        <button type="button" onclick={() => (editing = null)}>{t.cancel}</button>
        <button type="submit" class="primary" disabled={saving}>{t.save}</button>
      </div>
    </form>
  </Dialog>
{/if}

<style>
  section {
    display: grid;
    gap: 10px;
    margin-bottom: 16px;
  }
  .head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 8px;
  }
  h2 {
    font-size: 1.05rem;
    margin: 0;
  }
  p {
    margin: 0;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 8px;
  }
  li {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .about {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 6px;
    overflow-wrap: anywhere;
  }
  .badge {
    font-size: 0.75rem;
    padding: 1px 7px;
    border-radius: 999px;
    border: 1px solid var(--border);
    color: var(--muted);
  }
  form {
    display: grid;
    gap: 12px;
  }
  label {
    display: grid;
    gap: 4px;
  }
  .two {
    display: grid;
    gap: 12px;
    grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
  }
  .check {
    display: flex;
    align-items: flex-start;
    gap: 8px;
  }
  .check .hint {
    display: block;
  }
  .mono {
    font-family: var(--mono);
    font-size: 0.8rem;
  }
  .buttons {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }
</style>
