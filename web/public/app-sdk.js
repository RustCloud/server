// RustCloud app SDK for pages of WASM apps shown in the panel:
//
//   <script src="/app-sdk.js"></script>
//   const ctx = await rustcloud.context;          // { locale, theme, user, limits, file, layout, origin }
//   const res = await rustcloud.fetch('/notes');  // the app's own route, as the user
//   const folder = await rustcloud.requestAccess({ folder: 'Notes', write: true });
//   await rustcloud.share({ path: 'calendars/work', label: 'Work' });
//   rustcloud.setLayout('full');                  // the whole browser window, or 'panel'
//   rustcloud.close();                            // as the panel's close button
//   await rustcloud.openFrame({ url, fields: { access_token } });  // an approved service over the page
//   rustcloud.onFrameMessage((data) => …); rustcloud.postToFrame(data); rustcloud.closeFrame();
//
// A page opened for a file (`[[open_with]]` in the manifest) gets `?file=<id>`
// in its address and ctx.file = { id, name, mime_type, write }: the app has
// access to that file while the page is open.
//
// The page runs in a sandboxed <iframe> with a unique origin, so it can't send
// the user's cookie itself; the panel makes requests to the app's routes for
// it. Bodies may be text (JSON) or bytes: an ArrayBuffer, a typed array, a Blob
// or a File, up to what the app takes at once (ctx.limits.body, 16 MiB at ×1). Answers come as bytes, so res.text(), res.json()
// and res.blob() all work (a picture: URL.createObjectURL(await res.blob())).
// Headers the page may set: content-type, accept, range, if-match,
// if-none-match; it gets back content-type, content-disposition,
// content-range, etag and last-modified.
(() => {
  const pending = new Map();
  const frameListeners = [];
  let next = 1;
  let gotContext;
  const context = new Promise((resolve) => (gotContext = resolve));

  window.addEventListener('message', (event) => {
    if (event.source !== window.parent) return;
    const message = event.data;
    if (!message || typeof message !== 'object') return;
    if (message.rustcloud === 'context') {
      const { locale, theme, user, limits, file, layout, origin } = message;
      gotContext({ locale, theme, user, limits, file: file || null, layout, origin });
      return;
    }
    if (message.rustcloud === 'frame-message') {
      for (const listener of frameListeners) listener(message.data);
      return;
    }
    const request = pending.get(message.id);
    if (!request) return;
    pending.delete(message.id);
    if (message.rustcloud === 'access') {
      request.resolve(message.folder);
    } else if (message.rustcloud === 'shared' || message.rustcloud === 'frame-opened') {
      request.resolve();
    } else if (message.rustcloud === 'response') {
      const empty = message.status === 204 || message.status === 304;
      request.resolve(new Response(empty ? null : message.body, { status: message.status, headers: message.headers }));
    } else if (message.rustcloud === 'error') {
      request.reject(new Error(message.message));
    }
  });

  /** A body for the panel: text as it is, anything else as bytes it can take over. */
  async function bridgeBody(body, headers) {
    if (body === undefined || body === null || typeof body === 'string') return { body: body ?? null, transfer: [] };
    if (body instanceof Blob) {
      const named = Object.keys(headers).some((name) => name.toLowerCase() === 'content-type');
      if (body.type && !named) headers['content-type'] = body.type;
      body = await body.arrayBuffer();
    } else if (ArrayBuffer.isView(body)) {
      body = body.buffer.slice(body.byteOffset, body.byteOffset + body.byteLength);
    } else if (body instanceof ArrayBuffer) {
      body = body.slice(0);
    } else {
      throw new TypeError('rustcloud.fetch: the body must be text, an ArrayBuffer, a typed array or a Blob');
    }
    return { body, transfer: [body] };
  }

  /** Like fetch(), for a path of this app (`/notes`), as the logged-in user. */
  async function appFetch(path, init = {}) {
    const headers = { ...(init.headers || {}) };
    const { body, transfer } = await bridgeBody(init.body, headers);
    return new Promise((resolve, reject) => {
      const id = next++;
      pending.set(id, { resolve, reject });
      window.parent.postMessage(
        { rustcloud: 'fetch', id, method: init.method || 'GET', path, headers, body },
        '*',
        transfer,
      );
    });
  }

  /**
   * Asks the user for standing access to a folder: the panel offers to create
   * `folder` or to pick one. Resolves to { id, name } of the folder, or rejects
   * if the user declines. The app then reaches it through its `files` import.
   */
  function requestAccess({ folder, write = false } = {}) {
    return new Promise((resolve, reject) => {
      const id = next++;
      pending.set(id, { resolve, reject });
      window.parent.postMessage({ rustcloud: 'request-access', id, folder, write }, '*');
    });
  }

  /**
   * Opens the panel's dialog for sharing a folder of this app's data (`path`,
   * relative to the app's AppData) with other users, read-only or for
   * changes; `label` is what the user knows it as. Resolves when the dialog
   * is closed. Other users' apps reach the folder through the `shares` import.
   */
  function share({ path, label } = {}) {
    return new Promise((resolve, reject) => {
      const id = next++;
      pending.set(id, { resolve, reject });
      window.parent.postMessage({ rustcloud: 'share', id, path, label }, '*');
    });
  }

  /** Shows the page over the whole browser window ('full') or in the panel's page area ('panel'). */
  function setLayout(mode) {
    window.parent.postMessage({ rustcloud: 'layout', mode: mode === 'full' ? 'full' : 'panel' }, '*');
  }

  /** Closes the page as the panel's close button does: back to the files, or out of the full screen. */
  function close() {
    window.parent.postMessage({ rustcloud: 'close' }, '*');
  }

  /**
   * Shows a service over the page, outside its sandbox: an editor such as
   * Collabora, at an address the administrator approved for this app
   * (`permissions.frame`). `fields` are posted to `url` as a form (a WOPI
   * access token); `method: 'get'` opens it without a form.
   */
  function openFrame({ url, method = 'post', fields = {} } = {}) {
    return new Promise((resolve, reject) => {
      const id = next++;
      pending.set(id, { resolve, reject });
      window.parent.postMessage({ rustcloud: 'open-frame', id, url, method, fields }, '*');
    });
  }

  /** Sends a message to the service (its postMessage API), to its own origin only. */
  function postToFrame(data) {
    window.parent.postMessage({ rustcloud: 'frame-post', data }, '*');
  }

  /** Calls `listener(data)` for each message of the service. */
  function onFrameMessage(listener) {
    frameListeners.push(listener);
  }

  /** Takes the service away; the page shows again. */
  function closeFrame() {
    window.parent.postMessage({ rustcloud: 'close-frame' }, '*');
  }

  window.rustcloud = {
    fetch: appFetch,
    context,
    requestAccess,
    share,
    setLayout,
    close,
    openFrame,
    postToFrame,
    onFrameMessage,
    closeFrame,
  };
  window.parent.postMessage({ rustcloud: 'ready' }, '*');
})();
