<!-- Progress of running and finished uploads (bottom-right corner). -->
<script lang="ts" module>
  export interface UploadItem {
    id: number;
    name: string;
    progress: number;
    state: 'waiting' | 'running' | 'done' | 'failed';
    error?: string;
    cancel?: () => void;
  }
</script>

<script lang="ts">
  import { t } from '../lib/i18n';

  let { items, onclear }: { items: UploadItem[]; onclear: () => void } = $props();

  const running = $derived(items.some((item) => item.state === 'running' || item.state === 'waiting'));
</script>

{#if items.length}
  <aside class="uploads">
    <header>
      <strong>{t.uploads}</strong>
      {#if !running}<button type="button" class="small" onclick={onclear}>{t.close}</button>{/if}
    </header>
    <ul>
      {#each items as item (item.id)}
        <li>
          <div class="row">
            <span class="name">{item.name}</span>
            {#if item.state === 'waiting'}
              <span class="muted small">{t.uploadWaiting}</span>
            {:else if item.state === 'running'}
              <button type="button" class="small" onclick={() => item.cancel?.()}>{t.cancel}</button>
            {:else if item.state === 'done'}
              <span class="ok small">{t.uploadDone}</span>
            {:else}
              <span class="error small" title={item.error}>{t.uploadFailed}</span>
            {/if}
          </div>
          {#if item.state === 'running'}
            <progress max="1" value={item.progress}></progress>
          {:else if item.error}
            <div class="error small">{item.error}</div>
          {/if}
        </li>
      {/each}
    </ul>
  </aside>
{/if}

<style>
  .uploads {
    position: fixed;
    right: 16px;
    bottom: 16px;
    width: min(360px, calc(100vw - 32px));
    max-height: 50vh;
    overflow: auto;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 12px;
    box-shadow: 0 10px 30px rgb(0 0 0 / 15%);
    padding: 12px;
  }
  header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 8px;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 10px;
  }
  .row {
    display: flex;
    justify-content: space-between;
    gap: 8px;
    align-items: center;
  }
  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  progress {
    width: 100%;
    height: 6px;
    accent-color: var(--accent);
  }
</style>
