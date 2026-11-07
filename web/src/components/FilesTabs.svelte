<!-- Tabs of the files app. -->
<script lang="ts">
  import { t } from '../lib/i18n';
  import { href } from '../lib/router.svelte';

  let { active }: { active: 'files' | 'shared' | 'links' | 'search' | 'trash' } = $props();
</script>

<nav class="tabs">
  <a href={href('/files')} class:active={active === 'files'}>{t.myFiles}</a>
  <a href={href('/shared')} class:active={active === 'shared'}>{t.sharedWithMe}</a>
  <a href={href('/links')} class:active={active === 'links'}>{t.links}</a>
  <a href={href('/trash')} class:active={active === 'trash'}>{t.trash}</a>
</nav>
