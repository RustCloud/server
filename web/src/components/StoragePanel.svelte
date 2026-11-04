<!--
  Administration → Files: where files are kept, how full the disk is, and how
  fast it is. The speed is measured when the tab opens if the last test is
  older than an hour, or with the button.
-->
<script lang="ts">
  import { api } from '../lib/api';
  import { formatBytes, formatDate } from '../lib/format';
  import { errorText, locale, t } from '../lib/i18n';
  import { loadStorage, percentText, storage } from '../lib/storage.svelte';

  const AUTO_TEST_AFTER_MS = 60 * 60 * 1000;

  let testing = $state(false);
  let error = $state('');

  async function runSpeedTest() {
    testing = true;
    error = '';
    try {
      await api.speedTest();
      await loadStorage();
    } catch (e) {
      error = errorText(e);
    }
    testing = false;
  }

  loadStorage().then((report) => {
    const last = report?.speed_test ? Date.parse(report.speed_test.tested_at) : 0;
    if (report && Date.now() - last > AUTO_TEST_AFTER_MS) runSpeedTest();
  });

  const report = $derived(storage.report);
</script>

<section class="card storage">
  <h2>{t.storageTitle}</h2>
  {#if report}
    <p>
      <strong>{t.storageKinds[report.kind] ?? report.kind}</strong>
      <code>{report.location}</code>
    </p>

    {#if report.capacity && report.used_percent !== null}
      {@const used = report.capacity.total_bytes - report.capacity.free_bytes}
      <progress class:warn={report.nearly_full} max="100" value={report.used_percent}></progress>
      <p>
        {t.diskUsage(formatBytes(used), formatBytes(report.capacity.total_bytes), percentText(report.used_percent))},
        {t.diskFree(formatBytes(report.capacity.free_bytes))}
      </p>
      {#if report.nearly_full}<p class="warning">{t.nearlyFull(percentText(report.used_percent))}</p>{/if}
    {/if}
    <p class="muted">{t.storedFiles(formatBytes(report.stored_bytes), report.file_count)}</p>

    <h3>{t.speedTest}</h3>
    {#if report.speed_test}
      <p>
        {t.speedResult(report.speed_test.write_speed.toFixed(1), report.speed_test.read_speed.toFixed(1))}
        <br />
        <span class="muted small">
          {t.speedTestedAt(formatDate(report.speed_test.tested_at, locale), formatBytes(report.speed_test.bytes))}
        </span>
      </p>
    {:else if !testing}
      <p class="muted">{t.speedNever}</p>
    {/if}
    <button type="button" onclick={runSpeedTest} disabled={testing}>
      {testing ? t.speedTestRunning : t.speedTestRun}
    </button>
    {#if error}<p class="error">{error}</p>{/if}
  {:else}
    <p class="muted">{t.loading}</p>
  {/if}
</section>

<style>
  .storage {
    max-width: 640px;
    margin-bottom: 16px;
  }
  h2 {
    font-size: 1.05rem;
    margin: 0 0 8px;
  }
  h3 {
    font-size: 0.95rem;
    margin: 16px 0 4px;
  }
  p {
    margin: 6px 0;
  }
  code {
    font-family: var(--mono);
    font-size: 0.85rem;
    margin-left: 8px;
  }
  progress {
    width: 100%;
    height: 10px;
    accent-color: var(--accent);
  }
  progress.warn {
    accent-color: var(--danger);
  }
</style>
