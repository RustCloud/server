<!--
  Settings → Sessions: the browsers and client apps logged in to the
  account, with a button to end one or all the others.
-->
<script lang="ts">
  import { api, type SessionInfo } from '../lib/api';
  import { describeAgent, formatDate } from '../lib/format';
  import { errorText, locale, t } from '../lib/i18n';

  let sessions = $state<SessionInfo[]>([]);
  let message = $state('');
  let error = $state('');

  async function load() {
    try {
      sessions = await api.sessions();
    } catch (e) {
      error = errorText(e);
    }
  }

  async function end(session: SessionInfo) {
    error = message = '';
    try {
      await api.endSession(session.id);
      await load();
    } catch (e) {
      error = errorText(e);
    }
  }

  async function endOthers() {
    error = message = '';
    try {
      const { ended } = await api.logoutOthers();
      message = t.otherSessionsEnded(ended);
      await load();
    } catch (e) {
      error = errorText(e);
    }
  }

  const title = (s: SessionInfo) => (s.device ? (s.name ?? t.sessionApp) : s.name ? describeAgent(s.name) : '—');

  load();
</script>

<section class="card">
  <h2>{t.sessions}</h2>
  <p class="muted small">{t.sessionsHint}</p>
  <ul>
    {#each sessions as session (session.id)}
      <li>
        <div class="about">
          <span>
            {title(session)}
            {#if session.device}<span class="badge">{t.sessionApp}</span>{/if}
            {#if session.current}<span class="badge">{t.thisSession}</span>{/if}
          </span>
          <span class="muted small">
            {t.sessionUsed(formatDate(session.last_used_at ?? session.created_at, locale), session.ip)}
            {#if session.method?.startsWith('oidc:')}· {session.method.slice(5)}{/if}
          </span>
        </div>
        {#if !session.current}
          <button type="button" class="small" onclick={() => end(session)}>{t.endSession}</button>
        {/if}
      </li>
    {/each}
  </ul>
  {#if sessions.length > 1}
    <button type="button" onclick={endOthers}>{t.logoutOthers}</button>
  {/if}
  {#if message}<p class="small" role="status">{message}</p>{/if}
  {#if error}<p class="error small" role="alert">{error}</p>{/if}
</section>

<style>
  h2 {
    font-size: 1.05rem;
    margin: 0;
  }
  section {
    display: grid;
    gap: 10px;
    justify-items: start;
  }
  p {
    margin: 0;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    width: 100%;
    display: grid;
    gap: 8px;
  }
  li {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 8px;
  }
  .about {
    display: grid;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .badge {
    font-size: 0.75rem;
    padding: 1px 7px;
    margin-left: 4px;
    border-radius: 999px;
    border: 1px solid var(--border);
    color: var(--muted);
  }
</style>
