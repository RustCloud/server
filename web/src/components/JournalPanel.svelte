<!-- Administration → Journal: the activity log, newest first, with a filter and paging. -->
<script lang="ts">
  import { api, type JournalEntry } from '../lib/api';
  import { formatBytes, formatDate } from '../lib/format';
  import { errorText, journalText, locale, t } from '../lib/i18n';

  let entries = $state<JournalEntry[]>([]);
  let dropped = $state(0);
  let problems = $state(false);
  let query = $state('');
  let loading = $state(false);
  /** Whether older entries may exist (the last page was full). */
  let more = $state(false);
  let error = $state('');

  const PAGE_SIZE = 100;

  /** Loads the newest entries, or with `older` the page after the loaded ones. */
  async function load(older = false) {
    loading = true;
    try {
      const before = older ? entries[entries.length - 1]?.id : undefined;
      const page = await api.journal({ problems, q: query.trim(), before });
      entries = older ? [...entries, ...page.entries] : page.entries;
      dropped = page.dropped;
      more = page.entries.length === PAGE_SIZE;
      error = '';
    } catch (e) {
      error = errorText(e);
    }
    loading = false;
  }

  function search(event: SubmitEvent) {
    event.preventDefault();
    load();
  }

  /** Fields not already shown in the event text, e.g. a file size. */
  function extra(entry: JournalEntry): string {
    const shown = new Set(['name', 'new_name', 'account', 'section', 'write', 'read']);
    const known = entry.action !== null && entry.action in t.journalActions;
    return Object.entries(entry.details)
      .filter(([key]) => !known || !shown.has(key))
      .map(([key, value]) => {
        if (key === 'size' && typeof value === 'number') return formatBytes(value);
        return `${key}: ${typeof value === 'string' ? value : JSON.stringify(value)}`;
      })
      .join(', ');
  }

  load();
</script>

<section class="journal">
  <form class="filters" onsubmit={search}>
    <input type="search" placeholder={t.journalSearch} bind:value={query} />
    <label class="check"><input type="checkbox" bind:checked={problems} onchange={() => load()} /> {t.journalProblems}</label>
    <button type="submit" disabled={loading}>{t.journalRefresh}</button>
  </form>

  {#if error}<p class="error">{error}</p>{/if}
  {#if dropped}<p class="warning">{t.journalDropped(dropped)}</p>{/if}

  {#if entries.length}
    <div class="table-wrap">
      <table>
        <thead>
          <tr>
            <th>{t.journalTime}</th>
            <th>{t.journalUser}</th>
            <th>{t.journalAddress}</th>
            <th>{t.journalEvent}</th>
          </tr>
        </thead>
        <tbody>
          {#each entries as entry (entry.id)}
            <tr class={entry.level.toLowerCase()}>
              <td class="time">{formatDate(entry.time, locale)}</td>
              <td>{entry.username ?? ''}</td>
              <td class="ip">{entry.ip ?? ''}</td>
              <td>
                {#if entry.level !== 'INFO'}<span class="level">{entry.level}</span>{/if}
                {journalText(entry)}
                {#if extra(entry)}<div class="muted small details">{extra(entry)}</div>{/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
    {#if more}
      <button type="button" class="more" onclick={() => load(true)} disabled={loading}>{t.journalMore}</button>
    {/if}
  {:else if !loading}
    <p class="muted">{t.journalEmpty}</p>
  {/if}
</section>

<style>
  .journal {
    margin-bottom: 24px;
  }
  .filters {
    display: flex;
    flex-wrap: wrap;
    gap: 10px;
    align-items: center;
    margin-bottom: 12px;
  }
  .filters input[type='search'] {
    flex: 1;
    min-width: 200px;
    max-width: 360px;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 6px;
    color: var(--text);
  }
  .table-wrap {
    overflow-x: auto;
  }
  table {
    width: 100%;
    border-collapse: collapse;
    font-size: 0.9rem;
  }
  th {
    text-align: left;
    font-weight: 500;
    color: var(--muted);
    font-size: 0.85rem;
    padding: 6px 8px;
    border-bottom: 1px solid var(--border);
  }
  td {
    padding: 6px 8px;
    border-bottom: 1px solid var(--border-soft);
    vertical-align: top;
  }
  .time,
  .ip {
    white-space: nowrap;
    color: var(--muted);
  }
  .ip {
    font-family: var(--mono);
    font-size: 0.8rem;
  }
  .level {
    font-size: 0.75rem;
    font-weight: 600;
    padding: 1px 6px;
    border-radius: 4px;
    margin-right: 6px;
  }
  tr.warn .level {
    background: color-mix(in srgb, #d97706 20%, transparent);
  }
  tr.error .level {
    background: color-mix(in srgb, var(--danger) 25%, transparent);
  }
  tr.error td {
    color: var(--danger);
  }
  .details {
    overflow-wrap: anywhere;
  }
  .more {
    margin-top: 12px;
  }
</style>
