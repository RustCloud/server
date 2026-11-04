<!--
  A form for one settings section, drawn from the field descriptions the
  server sends. A new app's section shows up here without UI changes; adding
  its texts to i18n.ts makes the labels nicer than the raw keys.
-->
<script lang="ts">
  import { untrack } from 'svelte';
  import { api, type SettingsField, type SettingsSection } from '../lib/api';
  import { errorText, settingHint, settingLabel, t } from '../lib/i18n';

  let { section, onsaved }: { section: SettingsSection; onsaved: (saved: SettingsSection) => void } = $props();

  /** Form values; lists are edited as text, one item per line. Filled once:
   *  the parent recreates the form for another section. */
  let draft = $state(untrack(() => toDraft(section)));
  let message = $state('');
  let failed = $state(false);
  let saving = $state(false);

  function toDraft(s: SettingsSection): Record<string, string | boolean> {
    const values: Record<string, string | boolean> = {};
    for (const field of s.fields) {
      const value = s.values[field.key];
      if (field.kind === 'bool') values[field.key] = value === true;
      else if (field.kind === 'list') values[field.key] = ((value as string[] | null) ?? []).join('\n');
      else values[field.key] = value === null || value === undefined ? '' : String(value);
    }
    return values;
  }

  function fromDraft(field: SettingsField, value: string | boolean): unknown {
    if (field.kind === 'bool') return value;
    const text = String(value).trim();
    if (field.kind === 'list') return text.split('\n').map((line) => line.trim()).filter(Boolean);
    if (field.kind === 'number') return text === '' ? (field.optional ? null : undefined) : Number(text);
    return text;
  }

  async function save(event: SubmitEvent) {
    event.preventDefault();
    const values: Record<string, unknown> = {};
    for (const field of section.fields) values[field.key] = fromDraft(field, draft[field.key]);
    saving = true;
    try {
      const saved = await api.saveSettings(section.name, values);
      draft = toDraft(saved);
      onsaved(saved);
      failed = false;
      message = t.saved;
    } catch (e) {
      failed = true;
      message = errorText(e);
    }
    saving = false;
  }
</script>

<form class="card settings" onsubmit={save}>
  {#each section.fields as field (field.key)}
    {@const locked = section.locked.includes(field.key)}
    {@const id = `setting-${section.name}-${field.key}`}
    {@const hint = settingHint(section.name, field.key)}
    <div class="field">
      {#if field.kind === 'bool'}
        <label class="check" for={id}>
          <input {id} type="checkbox" bind:checked={draft[field.key] as boolean} disabled={locked} />
          {settingLabel(section.name, field.key)}
        </label>
      {:else}
        <label for={id}>{settingLabel(section.name, field.key)}</label>
        {#if field.kind === 'list'}
          <textarea {id} rows="3" bind:value={draft[field.key]} disabled={locked}></textarea>
        {:else if field.kind === 'number'}
          <div class="number">
            <input
              {id}
              type="number"
              min="0"
              bind:value={draft[field.key]}
              placeholder={field.optional ? t.unlimited : ''}
              required={!field.optional}
              disabled={locked}
            />
            {#if field.unit}<span class="muted">{t.units[field.unit] ?? field.unit}</span>{/if}
          </div>
        {:else}
          <input {id} bind:value={draft[field.key]} disabled={locked} />
        {/if}
      {/if}
      {#if hint}<p class="muted small hint">{hint}</p>{/if}
      {#if locked}<p class="muted small hint">{t.settingFromEnv}</p>{/if}
    </div>
  {/each}

  <div class="actions">
    <button type="submit" class="primary" disabled={saving}>{t.save}</button>
    {#if message}<span class="small" class:error={failed} class:ok={!failed}>{message}</span>{/if}
  </div>
</form>

<style>
  .settings {
    display: grid;
    gap: 18px;
    max-width: 640px;
  }
  .field {
    display: grid;
    gap: 4px;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 8px;
    color: var(--text);
    font-size: 0.95rem;
  }
  .number {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .number input {
    width: 160px;
  }
  textarea {
    font: inherit;
    font-family: var(--mono);
    font-size: 0.85rem;
    color: var(--text);
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 8px 10px;
    resize: vertical;
  }
  .hint {
    margin: 0;
  }
  .actions {
    display: flex;
    align-items: center;
    gap: 12px;
  }
</style>
