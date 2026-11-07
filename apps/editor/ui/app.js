// The editor page: one text file, opened from the files' "Edit as text".
// It runs in the panel's sandboxed <iframe>; every request goes through
// rustcloud.fetch. Changes are saved a moment after typing stops (and at
// once with Ctrl+S), only over the version that was read: if someone else
// saved in between, the user chooses what to keep.
(async () => {
  const TEXTS = {
    en: {
      title: 'Text editor',
      noFile: 'Open a text file from your files with “Edit as text”.',
      notText: 'This file is not UTF-8 text, so it can’t be edited here.',
      tooLarge: 'This file is too large to edit here.',
      notFound: 'The file is gone, or you no longer have access to it.',
      save: 'Save',
      saved: 'Saved',
      saving: 'Saving…',
      unsaved: 'Not saved yet',
      readOnly: 'Read only',
      wrap: 'Wrap',
      edit: 'Text',
      split: 'Both',
      preview: 'Preview',
      plain: 'Plain text',
      changed: 'Someone else saved this file meanwhile. Your text is still here.',
      keepMine: 'Save mine over it',
      takeTheirs: 'Load theirs',
      externalImagesBlocked: (n) => `Images from other sites hidden: ${n}.`,
      showExternalImages: 'Show them',
      lines: (n) => `${n} lines`,
      noHighlight: 'Large file: highlighting is off',
      failed: (message) => `Error: ${message}`,
    },
    ru: {
      title: 'Текстовый редактор',
      noFile: 'Откройте текстовый файл из файлов пунктом «Редактировать как текст».',
      notText: 'Это не текст в UTF-8 — здесь его не отредактировать.',
      tooLarge: 'Файл слишком большой, чтобы редактировать его здесь.',
      notFound: 'Файла нет или доступа к нему больше нет.',
      save: 'Сохранить',
      saved: 'Сохранено',
      saving: 'Сохранение…',
      unsaved: 'Не сохранено',
      readOnly: 'Только чтение',
      wrap: 'Перенос',
      edit: 'Текст',
      split: 'Рядом',
      preview: 'Просмотр',
      plain: 'Обычный текст',
      changed: 'Файл тем временем сохранил кто-то другой. Ваш текст остался здесь.',
      keepMine: 'Сохранить мой поверх',
      takeTheirs: 'Загрузить их версию',
      externalImagesBlocked: (n) => `Скрыто картинок с других сайтов: ${n}.`,
      showExternalImages: 'Показать',
      lines: (n) => `Строк: ${n}`,
      noHighlight: 'Большой файл: подсветка выключена',
      failed: (message) => `Ошибка: ${message}`,
    },
  };
  /** Saved this long after typing stops. */
  const SAVE_DELAY = 1000;
  /** Larger files are not highlighted: typing must stay quick. */
  const HIGHLIGHT_LIMIT = 512 * 1024;
  const LANGUAGE_NAMES = {
    js: 'JavaScript / TypeScript', rust: 'Rust', go: 'Go', java: 'Java / Kotlin', c: 'C / C++', csharp: 'C#',
    swift: 'Swift', php: 'PHP', dart: 'Dart', python: 'Python', ruby: 'Ruby', lua: 'Lua', shell: 'Shell',
    powershell: 'PowerShell', sql: 'SQL', json: 'JSON', yaml: 'YAML', toml: 'TOML / INI', env: '.env',
    markup: 'HTML / XML', css: 'CSS', markdown: 'Markdown', dockerfile: 'Dockerfile', makefile: 'Makefile',
    diff: 'Diff',
  };

  const context = await rustcloud.context;
  const t = TEXTS[context.locale] ?? TEXTS.en;
  document.documentElement.dataset.theme = context.theme;
  document.title = t.title;
  const app = document.getElementById('app');
  const { highlight, languageOf } = window.highlighter;

  const prefs = load('editor-prefs', { wrap: false });
  /** The open file: { id, name, mime_type, etag }. */
  let file = null;
  let language = null;
  let writable = false;
  let crlf = false;
  let bom = false;
  let dirty = false;
  let conflict = false;
  let saveTimer = null;
  let saving = Promise.resolve();
  let mode = 'edit';
  let externalImages = false;

  function load(key, fallback) {
    try {
      return { ...fallback, ...JSON.parse(localStorage.getItem(key) ?? '{}') };
    } catch {
      return fallback;
    }
  }

  function store(key, value) {
    try {
      localStorage.setItem(key, JSON.stringify(value));
    } catch {
      // A sandboxed page may have no storage: the choice lasts until closed.
    }
  }

  async function api(method, path, body) {
    const init = { method };
    if (body !== undefined) {
      init.body = typeof body === 'string' ? body : JSON.stringify(body);
      init.headers = { 'content-type': 'application/json' };
    }
    const response = await rustcloud.fetch('/api' + path, init);
    const data = await response.json().catch(() => ({}));
    if (!response.ok) {
      const error = new Error(data.error ?? response.statusText);
      error.status = response.status;
      throw error;
    }
    return data;
  }

  function el(tag, props = {}, ...children) {
    const node = document.createElement(tag);
    for (const [key, value] of Object.entries(props)) {
      if (key.startsWith('on')) node.addEventListener(key.slice(2), value);
      else if (key === 'class') node.className = value;
      else node[key] = value;
    }
    node.append(...children.filter((child) => child !== null && child !== undefined && child !== false));
    return node;
  }

  function message(text) {
    app.replaceChildren(el('div', { class: 'center' }, el('p', {}, text)));
  }

  // ----- the text: line endings and the byte order mark are kept as they were -----

  function fromFile(content) {
    bom = content.startsWith('﻿');
    if (bom) content = content.slice(1);
    crlf = content.includes('\r\n');
    return crlf ? content.replace(/\r\n/g, '\n') : content;
  }

  function toFile(text) {
    const content = crlf ? text.replace(/\n/g, '\r\n') : text;
    return bom ? '﻿' + content : content;
  }

  // ----- the page -----

  const textarea = el('textarea', {
    spellcheck: false,
    autocapitalize: 'off',
    autocomplete: 'off',
    wrap: 'off',
    oninput: () => {
      render();
      if (writable) changed();
    },
    onscroll: syncScroll,
    onkeydown: keydown,
  });
  const highlighted = el('code');
  const pre = el('pre', { ariaHidden: 'true' }, highlighted);
  const gutter = el('pre', { class: 'gutter', ariaHidden: 'true' });
  const code = el('div', { class: 'code' }, pre, textarea);
  const editor = el('div', { class: 'editor' }, gutter, code);
  const preview = el('article', { class: 'preview' });
  const status = el('span', { class: 'muted small' });
  const info = el('span', { class: 'muted small' });
  const banner = el('div');
  const panes = el('div', { class: 'panes' });
  const toolbar = el('div', { class: 'toolbar' });

  /** Lines the gutter numbers now; 0 to number them again. */
  let gutterLines = 0;
  let renderQueued = false;
  function render() {
    if (renderQueued) return;
    renderQueued = true;
    requestAnimationFrame(() => {
      renderQueued = false;
      const text = textarea.value;
      const plain = !language || text.length > HIGHLIGHT_LIMIT;
      code.classList.toggle('plain', plain);
      // The last line needs a character after it, or the highlight is a line short.
      if (!plain) highlighted.innerHTML = highlight(text, language) + '\n ';
      const lines = text.split('\n').length;
      if (gutterLines !== lines && !prefs.wrap) {
        gutter.textContent = Array.from({ length: lines }, (_, i) => i + 1).join('\n') + '\n ';
        gutterLines = lines;
      }
      info.textContent = text.length > HIGHLIGHT_LIMIT && language ? t.noHighlight : t.lines(lines);
      syncScroll();
      if (mode !== 'edit') renderPreviewSoon();
    });
  }

  function syncScroll() {
    pre.scrollTop = textarea.scrollTop;
    pre.scrollLeft = textarea.scrollLeft;
    gutter.scrollTop = textarea.scrollTop;
  }

  /** Tab indents with what the file uses; Ctrl+S saves now. */
  function keydown(event) {
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 's') {
      event.preventDefault();
      saveNow();
      return;
    }
    if (event.key === 'Tab' && !event.ctrlKey && !event.altKey && !event.metaKey && writable) {
      event.preventDefault();
      if (event.shiftKey) return;
      insert(indentUnit());
    }
  }

  function indentUnit() {
    if (/^\t/m.test(textarea.value) || language === 'makefile') return '\t';
    const spaces = textarea.value.match(/^( +)\S/m);
    return spaces && spaces[1].length <= 8 ? ' '.repeat(Math.min(spaces[1].length, 4)) : '  ';
  }

  /** Types text at the caret, so that undo takes it back. */
  function insert(text) {
    if (!document.execCommand('insertText', false, text)) {
      textarea.setRangeText(text, textarea.selectionStart, textarea.selectionEnd, 'end');
      textarea.dispatchEvent(new Event('input'));
    }
  }

  function setStatus(text) {
    status.textContent = text;
  }

  function button(label, onclick, extra = {}) {
    return el('button', { type: 'button', onclick, ...extra }, label);
  }

  function drawToolbar() {
    const languageSelect = el(
      'select',
      {
        ariaLabel: 'language',
        onchange: (event) => {
          language = event.target.value || null;
          drawToolbar();
          drawPanes();
          render();
        },
      },
      el('option', { value: '' }, t.plain),
      ...Object.entries(LANGUAGE_NAMES).map(([id, name]) =>
        el('option', { value: id, selected: id === language }, name),
      ),
    );
    const views =
      language === 'markdown'
        ? [
            button(t.edit, () => setMode('edit'), { className: mode === 'edit' ? 'on' : '' }),
            button(t.split, () => setMode('split'), { className: mode === 'split' ? 'on' : '' }),
            button(t.preview, () => setMode('preview'), { className: mode === 'preview' ? 'on' : '' }),
          ]
        : [];
    toolbar.replaceChildren(
      el('span', { class: 'name', title: file.name }, file.name),
      status,
      el('span', { class: 'spacer' }),
      info,
      languageSelect,
      ...views,
      button(t.wrap, toggleWrap, { className: prefs.wrap ? 'on' : '' }),
      writable ? button(t.save, saveNow, { className: 'primary', disabled: !dirty }) : null,
    );
  }

  function drawPanes() {
    const markdown = language === 'markdown';
    const shown = markdown ? mode : 'edit';
    panes.replaceChildren(...(shown === 'preview' ? [preview] : shown === 'split' ? [editor, preview] : [editor]));
    if (shown !== 'edit') renderPreviewSoon();
  }

  function setMode(next) {
    mode = next;
    drawToolbar();
    drawPanes();
  }

  function toggleWrap() {
    prefs.wrap = !prefs.wrap;
    store('editor-prefs', prefs);
    app.classList.toggle('wrap', prefs.wrap);
    textarea.wrap = prefs.wrap ? 'soft' : 'off';
    gutterLines = 0;
    drawToolbar();
    render();
  }

  let previewTimer = null;
  function renderPreviewSoon() {
    clearTimeout(previewTimer);
    previewTimer = setTimeout(renderPreview, 300);
  }

  async function renderPreview() {
    try {
      const response = await rustcloud.fetch('/api/preview' + (externalImages ? '?external_images=1' : ''), {
        method: 'POST',
        body: textarea.value,
      });
      const { html, blocked_images: blocked } = await response.json();
      preview.innerHTML = html;
      if (blocked) {
        const show = button(t.showExternalImages, () => {
          externalImages = true;
          renderPreview();
        });
        preview.prepend(el('p', { class: 'muted small' }, t.externalImagesBlocked(blocked), ' ', show));
      }
    } catch (error) {
      preview.textContent = t.failed(error.message);
    }
  }

  // ----- saving -----

  function changed() {
    dirty = true;
    setStatus(conflict ? '' : t.unsaved);
    drawToolbar();
    clearTimeout(saveTimer);
    if (!conflict) saveTimer = setTimeout(saveNow, SAVE_DELAY);
  }

  function saveNow() {
    clearTimeout(saveTimer);
    if (!writable || conflict) return saving;
    saving = saving.then(save);
    return saving;
  }

  async function save() {
    if (!dirty) return;
    const text = textarea.value;
    setStatus(t.saving);
    try {
      const saved = await api('PUT', `/file?id=${encodeURIComponent(file.id)}`, {
        content: toFile(text),
        etag: file.etag,
      });
      file.etag = saved.etag;
      // Typed on while it was saved: that waits for the next save.
      dirty = textarea.value !== text;
      setStatus(dirty ? t.unsaved : t.saved);
      if (dirty) saveTimer = setTimeout(saveNow, SAVE_DELAY);
    } catch (error) {
      if (error.status === 409) {
        showConflict();
      } else if (error.status === 403) {
        writable = false;
        textarea.readOnly = true;
        setStatus(t.readOnly);
      } else {
        setStatus(t.failed(error.message));
      }
    }
    drawToolbar();
  }

  function showConflict() {
    conflict = true;
    setStatus('');
    banner.replaceChildren(
      el(
        'p',
        { class: 'banner' },
        t.changed,
        button(t.keepMine, async () => {
          const current = await api('GET', `/file?id=${encodeURIComponent(file.id)}`);
          file.etag = current.etag;
          conflict = false;
          banner.replaceChildren();
          dirty = true;
          saveNow();
        }),
        button(t.takeTheirs, async () => {
          conflict = false;
          banner.replaceChildren();
          await open();
        }),
      ),
    );
  }

  // ----- opening -----

  async function open() {
    const id = context.file?.id ?? new URLSearchParams(location.search).get('file');
    if (!id) return message(t.noFile);
    try {
      const data = await api('GET', `/file?id=${encodeURIComponent(id)}`);
      file = { id: data.id, name: data.name, mime_type: data.mime_type, etag: data.etag };
      writable = context.file?.write ?? true;
      language = languageOf(file.name, file.mime_type);
      textarea.value = fromFile(data.content);
      textarea.readOnly = !writable;
      dirty = false;
      document.title = `${file.name} — ${t.title}`;
      setStatus(writable ? '' : t.readOnly);
      app.classList.toggle('wrap', prefs.wrap);
      textarea.wrap = prefs.wrap ? 'soft' : 'off';
      if (!app.firstChild || !app.contains(toolbar)) {
        app.replaceChildren(el('div', { class: 'shell' }, toolbar, banner, panes));
      }
      gutterLines = 0;
      drawToolbar();
      drawPanes();
      render();
      textarea.focus();
      textarea.setSelectionRange(0, 0);
    } catch (error) {
      if (error.status === 415) message(t.notText);
      else if (error.status === 413) message(t.tooLarge);
      else if (error.status === 404) message(t.notFound);
      else message(t.failed(error.message));
    }
  }

  await open();
})();
