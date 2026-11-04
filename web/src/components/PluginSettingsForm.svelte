<!--
  The server settings of one plugin, as its manifest declares them. Secrets
  are never shown: an empty field keeps the saved one, "Clear" removes it.
-->
<script lang="ts">
  import { untrack } from 'svelte';
  import { api, type Plugin, type ServerSetting } from '../lib/api';
  import { errorText, locale, t } from '../lib/i18n';

  let { plugin, onsaved }: { plugin: Plugin; onsaved: (saved: Plugin) => void } = $props();

  const declared: ServerSetting[] = $derived(plugin.manifest?.server_settings ?? []);

  /** What the fields show: booleans, numbers, and text (lists one item per line). */
  function draftOf(saved: Plugin): Record<string, unknown> {
    const draft: Record<string, unknown> = {};
    for (const setting of saved.manifest?.server_settings ?? []) {
      const value = saved.settings[setting.key];
      draft[setting.key] =
        setting.kind === 'bool'
          ? value === true
          : setting.kind === 'number'
            ? (value ?? null)
            : setting.kind === 'list'
              ? ((value as string[] | undefined) ?? []).join('\n')
              : setting.kind === 'secret'
                ? ''
                : (value ?? '');
    }
    return draft;
  }

  // The form starts from the saved values and is refreshed after saving, not while typing.
  let draft = $state(untrack(() => draftOf(plugin)));
  /** Secrets to remove on save. */
  let clear = $state<Record<string, boolean>>({});
  let message = $state('');
  let error = $state(false);

  const text = (names: Record<string, string> | undefined) => (names ? (names[locale] ?? names.en ?? '') : '');

  /** The values to send: empty fields clear a setting, an empty secret keeps the saved one. */
  function values(): Record<string, unknown> {
    const out: Record<string, unknown> = {};
    for (const setting of declared) {
      const value = draft[setting.key];
      if (setting.kind === 'secret') {
        if (clear[setting.key]) out[setting.key] = null;
        else if (value) out[setting.key] = value;
      } else if (setting.kind === 'list') {
        const lines = String(value ?? '')
          .split('\n')
          .map((line) => line.trim())
          .filter(Boolean);
        out[setting.key] = lines.length ? lines : null;
      } else if (setting.kind === 'number') {
        out[setting.key] = typeof value === 'number' && !Number.isNaN(value) ? value : null;
      } else if (setting.kind === 'text') {
        out[setting.key] = value === '' ? null : value;
      } else {
        out[setting.key] = value;
      }
    }
    return out;
  }

  async function save(event: SubmitEvent) {
    event.preventDefault();
    try {
      const saved = await api.savePluginSettings(plugin.id, values());
      draft = draftOf(saved);
      clear = {};
      error = false;
      message = t.pluginSettingsSaved;
      onsaved(saved);
    } catch (e) {
      error = true;
      message = errorText(e);
    }
  }
</script>

<form onsubmit={save}>
  {#each declared as setting (setting.key)}
    {#if setting.kind === 'bool'}
      <label class="check">
        <input type="checkbox" bind:checked={draft[setting.key] as boolean} />
        {text(setting.label)}
      </label>
    {:else}
      <label>
        {text(setting.label)}
        {#if setting.kind === 'number'}
          <span class="row">
            <input type="number" bind:value={draft[setting.key]} />
            {#if setting.unit}<span class="muted small">{t.units[setting.unit] ?? setting.unit}</span>{/if}
          </span>
        {:else if setting.kind === 'list'}
          <textarea rows="3" bind:value={draft[setting.key]}></textarea>
        {:else if setting.kind === 'secret'}
          {@const set = plugin.secrets_set.includes(setting.key)}
          <span class="row">
            <input
              type="password"
              autocomplete="new-password"
              placeholder={set && !clear[setting.key] ? t.pluginSecretSet : ''}
              disabled={clear[setting.key]}
              bind:value={draft[setting.key]}
            />
            {#if set}
              <span class="check small">
                <input type="checkbox" bind:checked={clear[setting.key]} />
                {t.pluginSecretClear}
              </span>
            {/if}
          </span>
        {:else}
          <input bind:value={draft[setting.key]} />
        {/if}
      </label>
    {/if}
    {#if setting.hint}<p class="muted small">{text(setting.hint)}</p>{/if}
  {/each}
  <div class="row">
    <button type="submit" class="primary">{t.save}</button>
    {#if message}<span class="small" class:error>{message}</span>{/if}
  </div>
</form>

<style>
  form {
    display: grid;
    gap: 8px;
  }
  label {
    display: grid;
    gap: 4px;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .row input {
    flex: 1;
    min-width: 0;
  }
  p {
    margin: -4px 0 0;
  }
</style>
