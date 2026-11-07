<!-- Administration → Users: account management. -->
<script lang="ts">
  import { api, type AccountUpdate, type User } from '../lib/api';
  import { formatDate } from '../lib/format';
  import { errorText, locale, t } from '../lib/i18n';
  import { session } from '../lib/session.svelte';

  const MIB = 1024 * 1024;

  let users = $state<User[]>([]);
  let error = $state('');

  let username = $state('');
  let password = $state('');
  let isAdmin = $state(false);

  async function refresh() {
    try {
      users = await api.users();
    } catch (e) {
      error = errorText(e);
    }
  }

  async function update(user: User, change: AccountUpdate) {
    try {
      await api.updateUser(user.id, change);
      error = '';
    } catch (e) {
      error = errorText(e);
    }
    await refresh();
  }

  async function create(event: SubmitEvent) {
    event.preventDefault();
    try {
      await api.createUser({ username, password, is_admin: isAdmin });
      username = password = '';
      isAdmin = false;
      error = '';
    } catch (e) {
      error = errorText(e);
    }
    await refresh();
  }

  function setQuota(user: User, value: string) {
    const mib = value.trim() === '' ? null : Math.max(0, Math.round(Number(value)));
    if (mib !== null && Number.isNaN(mib)) return;
    update(user, { quota_bytes: mib === null ? null : mib * MIB });
  }

  function resetPassword(user: User) {
    const newPassword = prompt(t.newPasswordFor(user.username));
    if (newPassword) update(user, { password: newPassword });
  }

  async function remove(user: User) {
    if (prompt(t.deleteAccountConfirm(user.username))?.trim().toLowerCase() !== user.username) return;
    try {
      await api.deleteUser(user.id);
      error = '';
    } catch (e) {
      error = errorText(e);
    }
    await refresh();
  }

  refresh();
</script>

{#if error}<p class="error">{error}</p>{/if}

<div class="table-wrap">
  <table>
    <thead>
      <tr>
        <th>{t.username}</th>
        <th>{t.displayName}</th>
        <th>{t.isAdmin}</th>
        <th>{t.disabled}</th>
        <th>{t.quotaMib}</th>
        <th>{t.created}</th>
        <th></th>
      </tr>
    </thead>
    <tbody>
      {#each users as user (user.id)}
        {@const isSelf = user.id === session.user?.id}
        <tr>
          <td>{user.username}{#if isSelf}<span class="muted small"> ({t.you})</span>{/if}</td>
          <td>{user.display_name}</td>
          <td>
            <input
              type="checkbox"
              checked={user.is_admin}
              disabled={isSelf}
              onchange={(e) => update(user, { is_admin: e.currentTarget.checked })}
              aria-label={t.isAdmin}
            />
          </td>
          <td>
            <input
              type="checkbox"
              checked={user.disabled}
              disabled={isSelf}
              onchange={(e) => update(user, { disabled: e.currentTarget.checked })}
              aria-label={t.disabled}
            />
          </td>
          <td>
            <input
              class="quota"
              type="number"
              min="0"
              placeholder={t.unlimited}
              value={user.quota_bytes === null ? '' : Math.round(user.quota_bytes / MIB)}
              onchange={(e) => setQuota(user, e.currentTarget.value)}
              aria-label={t.quotaMib}
            />
          </td>
          <td class="muted small">{formatDate(user.created_at, locale)}</td>
          <td>
            <div class="actions">
              <button type="button" class="small" onclick={() => resetPassword(user)}>{t.resetPassword}</button>
              {#if !isSelf}
                <button type="button" class="small danger" onclick={() => remove(user)}>{t.deleteAccount}</button>
              {/if}
            </div>
          </td>
        </tr>
      {/each}
    </tbody>
  </table>
</div>

<form class="card new-user" onsubmit={create}>
  <h2>{t.newUser}</h2>
  <label>{t.username}<input bind:value={username} required autocomplete="off" /></label>
  <label>{t.password}<input type="password" bind:value={password} minlength="8" required autocomplete="new-password" /></label>
  <label class="inline"><input type="checkbox" bind:checked={isAdmin} /> {t.isAdmin}</label>
  <button type="submit" class="primary">{t.create}</button>
</form>

<style>
  .table-wrap {
    overflow-x: auto;
  }
  .actions {
    display: flex;
    gap: 6px;
    white-space: nowrap;
  }
  table {
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
    padding: 8px;
    border-bottom: 1px solid var(--border-soft);
  }
  .quota {
    width: 110px;
  }
  .new-user {
    margin-top: 24px;
    display: grid;
    gap: 12px;
    max-width: 380px;
  }
  .new-user h2 {
    font-size: 1.05rem;
    margin: 0;
  }
  .new-user button {
    justify-self: start;
  }
  .inline {
    display: flex;
    align-items: center;
    gap: 8px;
  }
</style>
