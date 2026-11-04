<!-- Asks what to do with items dropped on a folder: move, copy or nothing. Shown where the drop happened. -->
<script lang="ts">
  import { t } from '../lib/i18n';
  import Icon from './Icon.svelte';

  let {
    x,
    y,
    count,
    target,
    onchoose,
  }: {
    x: number;
    y: number;
    count: number;
    /** Name of the folder the items were dropped on. */
    target: string;
    onchoose: (action: 'move' | 'copy' | null) => void;
  } = $props();

  let menu: HTMLDivElement;

  $effect(() => {
    menu.querySelector('button')?.focus();
  });

  function onwindowclick(event: MouseEvent) {
    if (!menu.contains(event.target as Node)) onchoose(null);
  }
</script>

<svelte:window onclick={onwindowclick} onkeydown={(e) => e.key === 'Escape' && onchoose(null)} />

<!-- Kept inside the window, near the pointer. -->
<div
  bind:this={menu}
  class="drop-menu menu-items"
  role="menu"
  style:left="{Math.min(x, window.innerWidth - 240)}px"
  style:top="{Math.min(y, window.innerHeight - 180)}px"
>
  <p class="muted small">{t.dropInto(count, target)}</p>
  <button type="button" role="menuitem" onclick={() => onchoose('move')}><Icon name="right" /> {t.move}</button>
  <button type="button" role="menuitem" onclick={() => onchoose('copy')}><Icon name="plus" /> {t.copy}</button>
  <button type="button" role="menuitem" onclick={() => onchoose(null)}><Icon name="close" /> {t.cancel}</button>
</div>

<style>
  .drop-menu {
    position: fixed;
    right: auto;
    z-index: 20;
    width: 230px;
  }
  p {
    margin: 2px 10px 6px;
    overflow-wrap: anywhere;
  }
</style>
