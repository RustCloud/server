<!-- What a visitor of a public link sees: a file to download, or a folder to browse. -->
<script lang="ts">
  import { api, ApiError, type FileNode, type PublicView } from '../lib/api';
  import { formatBytes, formatDate } from '../lib/format';
  import { errorText, locale, t } from '../lib/i18n';
  import { go, href } from '../lib/router.svelte';
  import { loadServerInfo } from '../lib/server.svelte';
  import FileList from '../components/FileList.svelte';
  import Icon from '../components/Icon.svelte';
  import Logo from '../components/Logo.svelte';
  import Preview from '../components/Preview.svelte';
  import PreviewContent from '../components/PreviewContent.svelte';
  import { previewKind } from '../lib/preview';

  let { token, nodeId }: { token: string; nodeId?: string } = $props();

  let view = $state<PublicView | null>(null);
  let gone = $state(false);
  let locked = $state(false);
  let password = $state('');
  let error = $state('');
  let uploaded = $state(0);
  let previewing = $state<FileNode | null>(null);

  async function load() {
    try {
      view = await api.publicView(token, nodeId);
      gone = locked = false;
    } catch (e) {
      locked = e instanceof ApiError && e.code === 'PASSWORD_REQUIRED';
      gone = !locked;
    }
  }

  $effect(() => {
    load();
  });
  loadServerInfo();

  async function unlock(event: SubmitEvent) {
    event.preventDefault();
    try {
      await api.unlockShare(token, password);
      password = error = '';
      await load();
    } catch (e) {
      error = errorText(e);
    }
  }

  async function upload(event: Event & { currentTarget: HTMLInputElement }) {
    const files = Array.from(event.currentTarget.files ?? []);
    event.currentTarget.value = '';
    error = '';
    for (const file of files) {
      try {
        await api.publicUpload(token, view?.current.id, file);
        uploaded += 1;
      } catch (e) {
        error = `${file.name}: ${errorText(e)}`;
        break;
      }
    }
    await load();
  }

  function open(node: FileNode) {
    if (node.is_dir) go(`/s/${token}/${node.id}`);
    else if (previewKind(node)) previewing = node;
    else location.href = api.publicContentUrl(token, node.id, true);
  }
</script>

<div class="public">
  <header>
    <span class="brand"><Logo size={26} /> {t.appName}</span>
    <span class="muted">{t.sharedWithYou}</span>
  </header>

  <main>
    {#if gone}
      <p class="card">{t.linkGone}</p>
    {:else if locked}
      <form class="card unlock" onsubmit={unlock}>
        <p>{t.linkLocked}</p>
        <label>
          {t.password}
          <!-- svelte-ignore a11y_autofocus -->
          <input type="password" autocomplete="current-password" bind:value={password} required autofocus />
        </label>
        <button type="submit" class="primary">{t.linkUnlock}</button>
        {#if error}<p class="error">{error}</p>{/if}
      </form>
    {:else if view}
      {#if view.current.is_dir}
        <nav class="breadcrumbs">
          <a href={href('/s/' + token)}><Icon name="folder" /> {view.shared.name}</a>
          {#each view.path.slice(1) as part (part.id)}
            <span>/</span><a href={href(`/s/${token}/${part.id}`)}>{part.name}</a>
          {/each}
          {#if view.current.id !== view.shared.id}<span>/</span><strong>{view.current.name}</strong>{/if}
          {#if view.can_upload}
            <label class="button small upload">
              <Icon name="upload" />
              {t.uploadHere}
              <input type="file" multiple onchange={upload} />
            </label>
          {/if}
          <a class="button small zip" href={api.publicZipUrl(token, view.current.id)} download>
            <Icon name="download" />
            {t.downloadZip}
          </a>
        </nav>
        {#if error}<p class="error">{error}</p>{/if}
        {#if uploaded}<p class="muted small">{t.uploadedCount(uploaded)}</p>{/if}
        <FileList nodes={view.children} onopen={open} thumbnail={(node) => api.publicThumbnailUrl(token, node)}>
          {#snippet actions(node)}
            {#if !node.is_dir}
              <a class="button small" href={api.publicContentUrl(token, node.id, true)} download>
                <Icon name="download" />
                {t.download}
              </a>
            {:else}
              <a class="button small" href={api.publicZipUrl(token, node.id)} download>
                <Icon name="download" />
                {t.downloadZip}
              </a>
            {/if}
          {/snippet}
        </FileList>
      {:else}
        <section class="card file">
          <h1><Icon name="file" size={22} /> {view.current.name}</h1>
          <p class="muted">{formatBytes(view.current.size)} · {formatDate(view.current.updated_at, locale)}</p>
          {#if previewKind(view.current)}
            <div class="inline-preview">
              <PreviewContent node={view.current} url={api.publicContentUrl(token, view.current.id)} />
            </div>
          {/if}
          <a class="button primary" href={api.publicContentUrl(token, view.current.id, true)} download>
            <Icon name="download" />
            {t.downloadFile}
          </a>
        </section>
      {/if}
      {#if previewing && view.current.is_dir}
        <Preview
          nodes={view.children}
          start={previewing}
          url={(node) => api.publicContentUrl(token, node.id)}
          downloadUrl={(node) => api.publicContentUrl(token, node.id, true)}
          onclose={() => (previewing = null)}
        />
      {/if}
      {#if view.expires_at}
        <p class="muted small">{t.linkExpires}: {formatDate(view.expires_at, locale)}</p>
      {/if}
    {:else}
      <p class="muted">{t.loading}</p>
    {/if}
  </main>
</div>

<style>
  header {
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 12px 20px;
    border-bottom: 1px solid var(--border);
    background: var(--surface);
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 8px;
    font-weight: 600;
    color: var(--accent);
  }
  main {
    max-width: 960px;
    margin: 0 auto;
    padding: 20px 16px;
  }
  .breadcrumbs {
    display: flex;
    gap: 6px;
    align-items: center;
    flex-wrap: wrap;
    margin-bottom: 12px;
  }
  .breadcrumbs a {
    display: inline-flex;
    gap: 6px;
    align-items: center;
    color: var(--text);
  }
  .breadcrumbs .upload {
    margin-left: auto;
    cursor: pointer;
  }
  .breadcrumbs .upload input {
    display: none;
  }
  .breadcrumbs .upload + .zip {
    margin-left: 0;
  }
  .breadcrumbs .zip {
    margin-left: auto;
  }
  .unlock {
    display: grid;
    gap: 10px;
    justify-items: start;
    max-width: 360px;
  }
  .file {
    display: grid;
    gap: 12px;
    justify-items: start;
  }
  .file h1 {
    display: flex;
    gap: 8px;
    align-items: center;
    font-size: 1.2rem;
    margin: 0;
    overflow-wrap: anywhere;
  }
  .inline-preview {
    width: 100%;
    max-height: 70vh;
    overflow: auto;
  }
</style>
