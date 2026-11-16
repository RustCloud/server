<!--
  Rings of the user's plugins over any page: a call or a message from someone.
  The text is the panel's — the plugin's name, the sender's username and what
  it is about — so a plugin can't make a ring look like the cloud speaking.
-->
<script lang="ts">
  import { appName } from '../lib/apps.svelte';
  import { appEvents, declineRing, forgetEndedRings, type Ring } from '../lib/appEvents.svelte';
  import { t } from '../lib/i18n';
  import { go } from '../lib/router.svelte';

  $effect(() => {
    const timer = setInterval(forgetEndedRings, 1000);
    return () => clearInterval(timer);
  });

  function open(ring: Ring) {
    appEvents.rings = appEvents.rings.filter((r) => r.id !== ring.id);
    go(`/app/${encodeURIComponent(ring.app)}?ring=${encodeURIComponent(ring.ref)}`);
  }
</script>

{#if appEvents.rings.length}
  <div class="rings" role="status" aria-live="polite">
    {#each appEvents.rings as ring (ring.id)}
      <div class="ring">
        <p>
          <strong>{appName(ring.app)}</strong> · {ring.from.username}: {t.ringKind[ring.kind] ?? ring.kind}
        </p>
        <div class="actions">
          <button type="button" class="primary small" onclick={() => open(ring)}>{t.ringOpen}</button>
          <button type="button" class="small" onclick={() => declineRing(ring)}>{t.ringDecline}</button>
        </div>
      </div>
    {/each}
  </div>
{/if}

<style>
  .rings {
    position: fixed;
    right: 16px;
    bottom: 16px;
    z-index: 50;
    display: grid;
    gap: 8px;
    max-width: min(360px, calc(100vw - 32px));
  }
  .ring {
    background: var(--surface, var(--bg));
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 12px;
    box-shadow: 0 6px 24px rgb(0 0 0 / 0.18);
  }
  p {
    margin: 0 0 8px;
    overflow-wrap: anywhere;
  }
  .actions {
    display: flex;
    gap: 8px;
  }
</style>
