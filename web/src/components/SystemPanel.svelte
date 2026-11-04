<!--
  Administration → Server: what the server runs on and how it is doing —
  version, system, processor, memory, database, the storage disk and the
  native tools plugins use.
-->
<script lang="ts">
  import { api, type FfmpegStatus, type OnnxStatus, type SystemInfo } from '../lib/api';
  import { formatBytes } from '../lib/format';
  import { errorText, t } from '../lib/i18n';
  import StoragePanel from './StoragePanel.svelte';

  let info = $state<SystemInfo | null>(null);
  let ffmpeg = $state<FfmpegStatus | null>(null);
  let onnx = $state<OnnxStatus | null>(null);
  let error = $state('');

  async function load() {
    try {
      [info, ffmpeg, onnx] = await Promise.all([api.system(), api.ffmpegStatus(), api.onnxStatus()]);
      error = '';
    } catch (e) {
      error = errorText(e);
    }
  }

  load();

  /** `used of total (percent%)`. */
  function share(used: number, total: number): string {
    return t.diskUsage(formatBytes(used), formatBytes(total), String(Math.round((used * 100) / total)));
  }
</script>

<div class="stack">
  <section class="card">
    <div class="title">
      <h2>{t.systemTitle}</h2>
      <button type="button" class="small" onclick={load}>{t.journalRefresh}</button>
    </div>
    {#if error}<p class="error">{error}</p>{/if}
    {#if info}
      <dl>
        <dt>{t.systemVersion}</dt>
        <dd>
          RustCloud {info.version}
          {#if info.revision}<span class="muted">· <code>{info.revision.slice(0, 7)}</code></span>{/if}
        </dd>
        <dt>{t.systemUptime}</dt>
        <dd>{t.duration(info.uptime_seconds)}</dd>
        <dt>{t.systemOs}</dt>
        <dd>
          {info.os ?? '—'}{info.kernel ? `, ${t.systemKernel} ${info.kernel}` : ''} · {info.arch}
          {#if info.container}<span class="muted">· {t.systemContainer}</span>{/if}
        </dd>
        <dt>{t.systemCpu}</dt>
        <dd>
          {t.systemCpus(info.cpus)}
          {#if info.load}<span class="muted">· {t.systemLoad(info.load.map((n) => n.toFixed(2)).join(' / '))}</span>{/if}
        </dd>
        {#if info.memory}
          {@const used = info.memory.total_bytes - info.memory.available_bytes}
          <dt>{t.systemMemory}</dt>
          <dd>
            <progress max={info.memory.total_bytes} value={used}></progress>
            {share(used, info.memory.total_bytes)}, {t.diskFree(formatBytes(info.memory.available_bytes))}
            {#if info.memory.swap_total_bytes > 0}
              <br /><span class="muted">
                {t.systemSwap(share(info.memory.swap_total_bytes - info.memory.swap_free_bytes, info.memory.swap_total_bytes))}
              </span>
            {/if}
          </dd>
        {/if}
        {#if info.container_memory}
          <dt>{t.systemContainerMemory}</dt>
          <dd>
            <progress max={info.container_memory.limit_bytes} value={info.container_memory.used_bytes}></progress>
            {share(info.container_memory.used_bytes, info.container_memory.limit_bytes)}
          </dd>
        {/if}
        <dt>{t.systemProcess}</dt>
        <dd>
          {info.process.memory_bytes !== null ? t.systemProcessMemory(formatBytes(info.process.memory_bytes)) : '—'}
          {#if info.process.threads !== null}<span class="muted">· {t.systemThreads(info.process.threads)}</span>{/if}
        </dd>
        <dt>{t.systemDatabase}</dt>
        <dd>
          {info.database.kind === 'postgres' ? 'PostgreSQL' : 'SQLite'}
          {info.database.version ?? ''}
          {#if info.database.size_bytes !== null}<span class="muted">· {formatBytes(info.database.size_bytes)}</span>{/if}
        </dd>
        {#if ffmpeg}
          <dt>use.ffmpeg</dt>
          <dd class:muted={!ffmpeg.available}>
            {#if ffmpeg.available}
              {t.ffmpegRuns(
                ffmpeg.ffmpeg ?? '?',
                Object.entries(ffmpeg.layers ?? {})
                  .filter(([, on]) => on)
                  .map(([layer]) => layer)
                  .join(', '),
              )}
              {#if ffmpeg.problem}{ffmpeg.problem}{/if}
            {:else}
              {t.ffmpegMissing(ffmpeg.problem ?? '')}
            {/if}
          </dd>
        {/if}
        {#if onnx}
          <dt>use.onnx</dt>
          <dd class:muted={!onnx.available}>
            {#if onnx.available}
              {t.onnxRuns(onnx.engine ?? '?', Math.round(onnx.keep_loaded_seconds / 60))}
              {t.onnxLoaded(onnx.loaded.map((model) => `${model.app_id}/${model.path}`))}
              {#if onnx.problem}{onnx.problem}{/if}
            {:else}
              {t.onnxMissing(onnx.problem ?? '')}
            {/if}
          </dd>
        {/if}
      </dl>
    {:else if !error}
      <p class="muted">{t.loading}</p>
    {/if}
  </section>

  <StoragePanel />
</div>

<style>
  .stack {
    display: grid;
    gap: 16px;
  }
  .title {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    margin-bottom: 8px;
  }
  h2 {
    font-size: 1.05rem;
    margin: 0;
  }
  dl {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 8px 16px;
    margin: 0;
  }
  dt {
    color: var(--muted);
  }
  dd {
    margin: 0;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  progress {
    display: block;
    width: min(320px, 100%);
    margin-bottom: 2px;
  }
  @media (max-width: 640px) {
    dl {
      grid-template-columns: 1fr;
      gap: 2px;
    }
    dd {
      margin-bottom: 8px;
    }
  }
</style>
