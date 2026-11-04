<!--
  A plugin's kinds of background jobs: how many are left and failed, and
  the batch size its manifest gives each. What a batch may use follows from
  the plugin's limits (the advanced settings).
-->
<script lang="ts">
  import type { Plugin } from '../lib/api';
  import { t } from '../lib/i18n';

  let { plugin }: { plugin: Plugin } = $props();

  const kinds = $derived(plugin.manifest?.jobs ?? []);
</script>

{#if kinds.length}
  <div class="jobs">
    <span class="small">{t.pluginJobs}:</span>
    {#each kinds as kind (kind.kind)}
      {@const counts = plugin.queue[kind.kind] ?? { waiting: 0, failed: 0 }}
      <div class="kind small">
        <strong>{kind.kind}</strong>
        <span class="muted">{t.pluginJobsLeft(counts.waiting, counts.failed)}</span>
        <span class="muted">{t.pluginJobsBatch(kind.batch)}</span>
      </div>
    {/each}
  </div>
{/if}

<style>
  .jobs {
    display: grid;
    gap: 4px;
  }
  .kind {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 8px;
  }
</style>
