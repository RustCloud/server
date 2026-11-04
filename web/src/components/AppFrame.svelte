<!--
  A WASM app's page in a sandboxed <iframe>, and the bridge that makes requests for it.
  With `?file=<id>` the page shows a file the app opens (`[[open_with]]`): the user's
  choice gives the app access to it while it is open, here or full screen.
-->
<script lang="ts">
  import { tick } from 'svelte';
  import { api, type FileNode, type Grant } from '../lib/api';
  import { mimeMatches, serviceOrigin } from '../lib/apps';
  import { apps, takeChosen } from '../lib/apps.svelte';
  import { allowedHeaders, ANSWER_HEADERS, appRequestUrl, BODY_LIMIT, requestBody, type BridgeRequest } from '../lib/bridge';
  import AccessDialog from './AccessDialog.svelte';
  import AppShareDialog from './AppShareDialog.svelte';
  import Dialog from './Dialog.svelte';
  import { errorText, locale, t } from '../lib/i18n';
  import { go } from '../lib/router.svelte';
  import { session } from '../lib/session.svelte';

  let { id, path, query, title }: { id: string; path: string; query: string; title: string } = $props();

  /** How often the open file's access is confirmed (the server ends it after 2 minutes without). */
  const KEEP_ALIVE_MS = 30_000;
  const REMEMBER = (appId: string) => `rustcloud.openWith.${appId}`;

  let frame = $state<HTMLIFrameElement>();
  /** The file this page opens, once the user agreed and the app got access to it. */
  let opened = $state<{ grant: Grant; fromList: boolean } | null>(null);
  /** The file waiting for the user's answer before the app gets it. */
  let asking = $state<{ node: FileNode; write: boolean; remember: boolean } | null>(null);
  let openError = $state('');
  /** The page asked for the whole window, or the app opens its files so. */
  let full = $state(false);
  /**
   * A service the page asked to show (an editor such as Collabora): over the
   * page, outside its sandbox, at an origin the administrator approved.
   */
  let service = $state<{ url: string; origin: string; method: 'get' | 'post'; fields: [string, string][] } | null>(
    null,
  );
  let serviceFrame = $state<HTMLIFrameElement>();
  let serviceForm = $state<HTMLFormElement>();
  const serviceName = $derived(`rustcloud-service-${id}`);
  const fileId = $derived(new URLSearchParams(query).get('file'));
  const ready = $derived(!fileId || opened !== null);
  /** A folder access request of the page waiting for the user's answer. */
  let accessRequest = $state<{ id: number; folder: string; write: boolean } | null>(null);
  /** A folder of the app's data the page offers to share, while the dialog is open. */
  let shareRequest = $state<{ id: number; path: string; label: string } | null>(null);
  const src = $derived(`/app/${encodeURIComponent(id)}/${path}${query ? '?' + query : ''}`);
  /** The most a request of the page may carry: what the app takes at once. */
  const bodyLimit = $derived(apps.limits[id]?.data_bytes ?? BODY_LIMIT);

  function reply(message: object, transfer: Transferable[] = []) {
    // The page's origin is unique ("null"), so it can only be addressed as "*";
    // the message goes to this frame's window only.
    frame?.contentWindow?.postMessage(message, '*', transfer);
  }

  function onMessage(event: MessageEvent) {
    // What the service says goes to the page, which knows its language.
    if (service && serviceFrame && event.source === serviceFrame.contentWindow) {
      if (event.origin === service.origin) reply({ rustcloud: 'frame-message', data: event.data });
      return;
    }
    if (!frame || event.source !== frame.contentWindow) return;
    const message = event.data;
    if (!message || typeof message !== 'object') return;
    if (message.rustcloud === 'ready') {
      const theme = matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
      const user = session.user && { username: session.user.username, display_name: session.user.display_name };
      const file = opened && {
        id: opened.grant.node.id,
        name: opened.grant.node.name,
        mime_type: opened.grant.node.mime_type,
        write: opened.grant.write,
      };
      const layout = full ? 'full' : 'panel';
      // The panel's origin: the page's own is opaque ("null"); a service needs it (WOPI's PostMessageOrigin).
      const origin = location.origin;
      reply({ rustcloud: 'context', locale, theme, user, limits: { body: bodyLimit }, file, layout, origin });
    } else if (message.rustcloud === 'open-frame' && typeof message.id === 'number') {
      openService(message);
    } else if (message.rustcloud === 'frame-post' && service) {
      serviceFrame?.contentWindow?.postMessage(message.data, service.origin);
    } else if (message.rustcloud === 'close-frame') {
      service = null;
    } else if (message.rustcloud === 'layout') {
      full = message.mode === 'full';
    } else if (message.rustcloud === 'close') {
      close();
    } else if (message.rustcloud === 'fetch' && typeof message.id === 'number') {
      forward(message as BridgeRequest);
    } else if (message.rustcloud === 'request-access' && typeof message.id === 'number') {
      const folder = typeof message.folder === 'string' && message.folder.trim() ? message.folder.trim() : title;
      if (accessRequest) {
        reply({ rustcloud: 'error', id: message.id, message: 'Another request is waiting for the user' });
      } else {
        accessRequest = { id: message.id, folder, write: message.write === true };
      }
    } else if (message.rustcloud === 'share' && typeof message.id === 'number') {
      if (typeof message.path !== 'string' || !message.path) {
        reply({ rustcloud: 'error', id: message.id, message: 'share needs the path of a folder' });
      } else if (shareRequest || accessRequest) {
        reply({ rustcloud: 'error', id: message.id, message: 'Another request is waiting for the user' });
      } else {
        const label = typeof message.label === 'string' && message.label.trim() ? message.label.trim() : title;
        shareRequest = { id: message.id, path: message.path, label };
      }
    }
  }

  // ----- a service shown for the page -----

  /**
   * Shows the service at `url`: at an origin the administrator approved for
   * the app, or at a path of this server that goes to the app's service.
   */
  async function openService(message: { id: number; url?: unknown; method?: unknown; fields?: unknown }) {
    const origin = serviceOrigin(String(message.url), apps.frames[id] ?? [], apps.proxies[id] ?? [], location.origin);
    if (!origin) {
      reply({ rustcloud: 'error', id: message.id, message: `${String(message.url)} is not a service of this app` });
      return;
    }
    const fields = Object.entries(message.fields && typeof message.fields === 'object' ? message.fields : {})
      .filter((field): field is [string, string] => typeof field[1] === 'string');
    const method = message.method === 'get' ? 'get' : 'post';
    service = { url: String(message.url), origin, method, fields };
    full = true;
    await tick();
    // A form posts the fields (a WOPI access token) into the frame, as the service expects.
    serviceForm?.submit();
    reply({ rustcloud: 'frame-opened', id: message.id });
  }

  // ----- the opened file -----

  $effect(() => {
    // The list of apps comes after login; the file is opened once.
    if (fileId && apps.manifests.length && opened?.grant.node.id !== fileId) openFile(fileId);
  });

  async function openFile(fileId: string) {
    openError = '';
    const entries = apps.manifests.find((app) => app.id === id)?.open_with ?? [];
    try {
      const node = (await api.details(fileId)).node;
      const opener = entries.find((open) => '/' + path === open.page && mimeMatches(open.mime, node.mime_type));
      if (!opener) {
        openError = t.appCannotOpen;
        return;
      }
      full = (opener.layout ?? 'full') === 'full';
      const write = opener.write ?? false;
      const fromList = takeChosen(id, fileId);
      if (fromList || remembered()) await grantFile(node, write, fromList);
      else asking = { node, write, remember: false };
    } catch (e) {
      openError = errorText(e);
    }
  }

  async function grantFile(node: FileNode, write: boolean, fromList: boolean) {
    const grant = await api.createGrant({ app_id: id, node_id: node.id, write, kind: 'session' });
    opened = { grant, fromList };
  }

  async function agree() {
    if (!asking) return;
    const { node, write, remember } = asking;
    asking = null;
    if (remember) {
      try {
        localStorage.setItem(REMEMBER(id), '1');
      } catch {
        // Asked again next time.
      }
    }
    try {
      await grantFile(node, write, false);
    } catch (e) {
      openError = errorText(e);
    }
  }

  function remembered(): boolean {
    try {
      return localStorage.getItem(REMEMBER(id)) === '1';
    } catch {
      return false;
    }
  }

  // The access lasts while the page is open: confirmed every 30 s, ended a
  // minute after leaving (an editor the app works with may still be saving).
  $effect(() => {
    const grant = opened?.grant;
    if (!grant) return;
    const timer = setInterval(() => api.keepGrant(grant.id).catch(() => {}), KEEP_ALIVE_MS);
    return () => {
      clearInterval(timer);
      api.closeGrant(grant.id).catch(() => {});
    };
  });

  /**
   * The close button: back to where the file was opened from (the files list,
   * or the file's folder); a page without a file leaves the full screen.
   */
  function close() {
    const node = opened?.grant.node ?? asking?.node;
    if (!fileId) {
      service = null;
      full = false;
    }
    else if (opened?.fromList) history.back();
    else go(node?.parent_id ? '/files/' + node.parent_id : '/files');
  }

  function shareClosed() {
    if (!shareRequest) return;
    reply({ rustcloud: 'shared', id: shareRequest.id });
    shareRequest = null;
  }

  function accessGranted(grant: Grant) {
    if (!accessRequest) return;
    reply({ rustcloud: 'access', id: accessRequest.id, folder: { id: grant.node.id, name: grant.node.name } });
    accessRequest = null;
  }

  function accessDenied() {
    if (!accessRequest) return;
    reply({ rustcloud: 'error', id: accessRequest.id, message: 'The user did not give access' });
    accessRequest = null;
  }

  async function forward(request: BridgeRequest) {
    const url = appRequestUrl(id, request.path, location.origin);
    if (!url) {
      reply({ rustcloud: 'error', id: request.id, message: 'Only the app’s own routes can be requested' });
      return;
    }
    const body = requestBody(request.body, bodyLimit);
    if ('error' in body) {
      reply({ rustcloud: 'error', id: request.id, message: body.error });
      return;
    }
    try {
      const response = await fetch(url, {
        method: typeof request.method === 'string' ? request.method : 'GET',
        headers: { ...allowedHeaders(request.headers), 'X-Requested-With': 'rustcloud' },
        body: body.body ?? undefined,
        credentials: 'same-origin',
      });
      const headers: Record<string, string> = {};
      for (const name of ANSWER_HEADERS) {
        const value = response.headers.get(name);
        if (value !== null) headers[name] = value;
      }
      // Bytes, whatever the answer is: the page reads them as text, JSON or a Blob.
      const answer = await response.arrayBuffer();
      reply({ rustcloud: 'response', id: request.id, status: response.status, headers, body: answer }, [answer]);
    } catch (e) {
      reply({ rustcloud: 'error', id: request.id, message: String(e) });
    }
  }
</script>

<svelte:window onmessage={onMessage} />

<div class="frame" class:full>
  {#if openError}
    <p class="error message">{openError}</p>
  {:else if ready}
    <iframe bind:this={frame} {src} {title} sandbox="allow-scripts allow-forms allow-downloads"></iframe>
  {/if}
  {#if service}
    <form bind:this={serviceForm} method={service.method} action={service.url} target={serviceName} hidden>
      {#each service.fields as [name, value] (name)}<input type="hidden" {name} {value} />{/each}
    </form>
    <iframe
      bind:this={serviceFrame}
      class="service"
      name={serviceName}
      title={title}
      allow="clipboard-read; clipboard-write; fullscreen"
      sandbox="allow-scripts allow-same-origin allow-forms allow-popups allow-popups-to-escape-sandbox allow-modals allow-downloads"
    ></iframe>
  {/if}
  {#if full}
    <button type="button" class="close" aria-label={t.close} title={t.close} onclick={close}>×</button>
  {/if}
</div>

{#if asking}
  <Dialog title={t.openFileTitle} onclose={close}>
    <p>{t.openFileText(title, asking.node.name, asking.write)}</p>
    <label class="small"><input type="checkbox" bind:checked={asking.remember} /> {t.rememberForApp}</label>
    <div class="buttons">
      <button type="button" onclick={close}>{t.cancel}</button>
      <button type="button" class="primary" onclick={agree}>{t.open}</button>
    </div>
  </Dialog>
{/if}

{#if accessRequest}
  <AccessDialog
    appId={id}
    appName={title}
    folderName={accessRequest.folder}
    write={accessRequest.write}
    ongranted={accessGranted}
    onclose={accessDenied}
  />
{/if}

{#if shareRequest}
  <AppShareDialog appId={id} appName={title} path={shareRequest.path} label={shareRequest.label} onclose={shareClosed} />
{/if}

<style>
  .frame {
    position: relative;
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  /* The whole browser window; only the close button stays over the page. */
  .frame.full {
    position: fixed;
    inset: 0;
    z-index: 100;
    background: var(--bg);
  }
  /* The service covers the page that asked for it. */
  .service {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    border: 0;
    background: var(--bg);
  }
  .close {
    z-index: 1;
    position: absolute;
    top: 8px;
    right: 8px;
    width: 32px;
    height: 32px;
    padding: 0;
    border-radius: 50%;
    font-size: 1.3rem;
    line-height: 1;
    opacity: 0.6;
    box-shadow: 0 2px 8px rgb(0 0 0 / 0.2);
  }
  .close:hover,
  .close:focus-visible {
    opacity: 1;
  }
  .message {
    margin: 24px;
  }
  .buttons {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 16px;
  }
  iframe {
    display: block;
    flex: 1;
    min-height: 0;
    width: 100%;
    border: 0;
    background: var(--bg);
  }
</style>
