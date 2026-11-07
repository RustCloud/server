<!--
  Allowing a client app (OAuth 2.0 with PKCE for native apps): the app opened
  /api/v1/auth/authorize in the browser, which led here. Allowing gets a
  one-time code from the server and sends the browser to the app's redirect
  URI with it — a loopback address or the app's own scheme, so the code stays
  on this device.
-->
<script lang="ts">
  import { api } from '../lib/api';
  import { errorText, t } from '../lib/i18n';
  import { route } from '../lib/router.svelte';
  import { session } from '../lib/session.svelte';

  const q = route.query;
  const clientId = q.get('client_id')?.trim() ?? '';
  const deviceName = q.get('device_name')?.trim() || undefined;
  const redirectUri = q.get('redirect_uri') ?? '';
  const challenge = q.get('code_challenge') ?? '';
  const method = q.get('code_challenge_method') ?? '';
  const appState = q.get('state');

  /** The redirect URI with these answers, or `null` if it isn't a URL. */
  function answer(params: Record<string, string>): string | null {
    try {
      const url = new URL(redirectUri);
      for (const [key, value] of Object.entries(params)) url.searchParams.set(key, value);
      if (appState !== null) url.searchParams.set('state', appState);
      return url.toString();
    } catch {
      return null;
    }
  }

  const valid = !q.get('invalid') && clientId !== '' && challenge !== '' && answer({}) !== null;
  const appName = deviceName ? `${deviceName} (${clientId})` : clientId;
  const where = (() => {
    try {
      const url = new URL(redirectUri);
      return url.protocol === 'http:' ? t.connectLoopback : t.connectScheme(url.protocol.slice(0, -1));
    } catch {
      return '';
    }
  })();

  let done = $state<'allowed' | 'denied' | null>(null);
  let error = $state('');
  let busy = $state(false);

  async function allow() {
    busy = true;
    error = '';
    try {
      const { code } = await api.authorize({
        client_id: clientId,
        device_name: deviceName,
        redirect_uri: redirectUri,
        code_challenge: challenge,
        code_challenge_method: method,
      });
      done = 'allowed';
      location.href = answer({ code }) ?? '';
    } catch (e) {
      error = errorText(e);
    } finally {
      busy = false;
    }
  }

  function deny() {
    done = 'denied';
    const url = answer({ error: 'access_denied' });
    if (url) location.href = url;
  }
</script>

<div class="connect card">
  {#if !valid}
    <p class="error" role="alert">{t.connectInvalid}</p>
  {:else if done === 'allowed'}
    <p role="status">{t.connectDone}</p>
  {:else if done === 'denied'}
    <p role="status">{t.connectDenied}</p>
  {:else}
    <h1>{t.connectTitle}</h1>
    <p><strong>{t.connectAsks(appName)}</strong> {t.connectWhere(where)}</p>
    <p class="muted small">{t.connectCan}</p>
    <p class="warning small">{t.connectWarning}</p>
    <p class="muted small">{t.connectAs(session.user?.username ?? '')}</p>
    {#if error}<p class="error small" role="alert">{error}</p>{/if}
    <div class="buttons">
      <button type="button" onclick={deny}>{t.connectDeny}</button>
      <button type="button" class="primary" disabled={busy} onclick={allow}>{t.connectAllow}</button>
    </div>
  {/if}
</div>

<style>
  .connect {
    max-width: 520px;
    margin: 32px auto;
    display: grid;
    gap: 12px;
  }
  h1 {
    font-size: 1.3rem;
    margin: 0;
  }
  p {
    margin: 0;
  }
  .buttons {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }
</style>
