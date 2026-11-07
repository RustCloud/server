<!-- Administration → Groups: groups of users that files are shared with. -->
<script lang="ts">
  import { api, type Group, type User } from '../lib/api';
  import { errorText, t } from '../lib/i18n';
  import Icon from '../components/Icon.svelte';

  let groups = $state<Group[]>([]);
  let users = $state<User[]>([]);
  let name = $state('');
  let error = $state('');

  async function refresh() {
    try {
      [groups, users] = await Promise.all([api.groups(), api.users()]);
    } catch (e) {
      error = errorText(e);
    }
  }

  async function run(action: () => Promise<unknown>) {
    try {
      await action();
      error = '';
    } catch (e) {
      error = errorText(e);
    }
    await refresh();
  }

  function create(event: SubmitEvent) {
    event.preventDefault();
    run(async () => {
      await api.createGroup(name);
      name = '';
    });
  }

  function rename(group: Group) {
    const newName = prompt(t.rename, group.name);
    if (newName && newName !== group.name) run(() => api.renameGroup(group.id, newName));
  }

  function remove(group: Group) {
    if (confirm(t.deleteGroupConfirm(group.name))) run(() => api.deleteGroup(group.id));
  }

  refresh();

  /** Users not yet in the group. */
  function candidates(group: Group) {
    return users.filter((u) => !group.members.some((m) => m.id === u.id));
  }
</script>

{#if error}<p class="error">{error}</p>{/if}

{#each groups as group (group.id)}
  <section class="card group">
    <header>
      <strong>{group.name}</strong>
      <span class="buttons">
        <button type="button" class="small" onclick={() => rename(group)}>{t.rename}</button>
        <button type="button" class="small danger" onclick={() => remove(group)}>{t.delete}</button>
      </span>
    </header>
    <div class="members">
      {#each group.members as member (member.id)}
        <span class="member">
          {member.username}
          <button
            type="button"
            class="icon"
            aria-label={t.delete}
            onclick={() => run(() => api.removeMember(group.id, member.id))}>×</button
          >
        </span>
      {/each}
      {#if candidates(group).length > 0}
        <details class="menu add-member">
          <summary class="button small"><Icon name="plus" /> {t.addMember}</summary>
          <div class="menu-items">
            {#each candidates(group) as user (user.id)}
              <button
                type="button"
                onclick={(e) => {
                  (e.currentTarget as HTMLElement).closest('details')?.removeAttribute('open');
                  run(() => api.addMember(group.id, user.id));
                }}
              >
                <Icon name="user" />
                {user.username}
              </button>
            {/each}
          </div>
        </details>
      {/if}
    </div>
  </section>
{/each}

<form class="card new-group" onsubmit={create}>
  <input placeholder={t.newGroup} bind:value={name} required aria-label={t.newGroup} />
  <button type="submit" class="primary">{t.create}</button>
</form>

<style>
  .group {
    display: grid;
    gap: 10px;
    margin-bottom: 12px;
  }
  header {
    display: flex;
    justify-content: space-between;
    align-items: center;
  }
  .buttons {
    display: flex;
    gap: 6px;
  }
  .members {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: center;
  }
  .member {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 2px 4px 2px 10px;
    border-radius: 999px;
    background: var(--border-soft);
  }
  .member .icon {
    border: 0;
    background: none;
    cursor: pointer;
    color: var(--muted);
  }
  .add-member .menu-items {
    left: 0;
    right: auto;
    max-height: 260px;
    overflow-y: auto;
  }
  .new-group {
    display: flex;
    gap: 8px;
  }
</style>
