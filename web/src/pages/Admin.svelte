<!-- Administration: the server's state, users, and one tab per settings section of the server and its apps. -->
<script lang="ts">
  import { api, type SettingsSection } from '../lib/api';
  import { errorText, sectionTitle, t } from '../lib/i18n';
  import { href } from '../lib/router.svelte';
  import type { Component } from 'svelte';
  import SettingsForm from '../components/SettingsForm.svelte';
  import JournalPanel from '../components/JournalPanel.svelte';
  import OidcPanel from '../components/OidcPanel.svelte';
  import PluginsPanel from '../components/PluginsPanel.svelte';
  import SystemPanel from '../components/SystemPanel.svelte';
  import AdminUsers from './AdminUsers.svelte';

  /** `system`, `users` or the name of a settings section. */
  let { tab = 'system' }: { tab?: string } = $props();

  let sections = $state<SettingsSection[]>([]);
  let error = $state('');

  api
    .settings()
    .then((loaded) => (sections = loaded))
    .catch((e) => (error = errorText(e)));

  const section = $derived(sections.find((s) => s.name === tab));

  /** Extra panels shown above a section's settings (later e.g. S3 storages, call servers). */
  const panels: Record<string, Component> = { accounts: OidcPanel, plugins: PluginsPanel, journal: JournalPanel };
  const Panel = $derived(panels[tab]);
</script>

<h1>{t.administration}</h1>
<nav class="tabs">
  <a href={href('/admin/system')} class:active={tab === 'system'}>{t.serverTab}</a>
  <a href={href('/admin/users')} class:active={tab === 'users'}>{t.users}</a>
  {#each sections as s (s.name)}
    <a href={href('/admin/' + s.name)} class:active={tab === s.name}>{sectionTitle(s.name)}</a>
  {/each}
</nav>

{#if error}<p class="error">{error}</p>{/if}
{#if tab === 'system'}
  <SystemPanel />
{:else if tab === 'users'}
  <AdminUsers />
{:else if section}
  {#if Panel}<Panel />{/if}
  {#key section.name}
    <SettingsForm {section} onsaved={(saved) => (sections = sections.map((s) => (s.name === saved.name ? saved : s)))} />
  {/key}
{/if}
