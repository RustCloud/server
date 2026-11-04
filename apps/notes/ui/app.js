// The Notes page: a list of notes and a Markdown editor. It runs in the
// panel's sandboxed <iframe>; every request goes through rustcloud.fetch.
(async () => {
  const TEXTS = {
    en: {
      title: 'Notes',
      choose: 'Choose where to keep your notes',
      chooseHint: 'Notes are Markdown files in one folder of your files; you can open them anywhere.',
      chooseButton: 'Choose a folder',
      folderName: 'Notes',
      denied: 'Notes need a folder to work.',
      search: 'Search notes',
      newNote: 'New',
      newName: 'New note',
      empty: 'No notes yet.',
      nothingFound: 'Nothing found.',
      pick: 'Choose a note or create a new one.',
      edit: 'Edit',
      preview: 'Preview',
      externalImagesBlocked: (n) => `Images from other sites hidden: ${n}.`,
      showExternalImages: 'Show them',
      delete: 'Delete',
      cancel: 'Cancel',
      confirmDelete: (title) => `Delete “${title}”?`,
      saved: 'Saved',
      saving: 'Saving…',
      unsaved: 'Not saved yet',
      changed: 'This note was changed elsewhere. Your text is kept here; reload to see the other version.',
      reload: 'Reload',
      failed: (message) => `Error: ${message}`,
    },
    ru: {
      title: 'Заметки',
      choose: 'Где хранить заметки?',
      chooseHint: 'Заметки — это Markdown-файлы в одной папке ваших файлов; их можно открыть чем угодно.',
      chooseButton: 'Выбрать папку',
      folderName: 'Заметки',
      denied: 'Заметкам нужна папка.',
      search: 'Поиск по заметкам',
      newNote: 'Новая',
      newName: 'Новая заметка',
      empty: 'Заметок пока нет.',
      nothingFound: 'Ничего не найдено.',
      pick: 'Выберите заметку или создайте новую.',
      edit: 'Текст',
      preview: 'Просмотр',
      externalImagesBlocked: (n) => `Скрыто картинок с других сайтов: ${n}.`,
      showExternalImages: 'Показать',
      delete: 'Удалить',
      cancel: 'Отмена',
      confirmDelete: (title) => `Удалить «${title}»?`,
      saved: 'Сохранено',
      saving: 'Сохранение…',
      unsaved: 'Не сохранено',
      changed: 'Заметку изменили в другом месте. Ваш текст остался здесь; обновите, чтобы увидеть другую версию.',
      reload: 'Обновить',
      failed: (message) => `Ошибка: ${message}`,
    },
  };
  const SAVE_DELAY = 800;

  const context = await rustcloud.context;
  const t = TEXTS[context.locale] ?? TEXTS.en;
  document.documentElement.dataset.theme = context.theme;
  document.title = t.title;
  const app = document.getElementById('app');

  /** The open note: { id, title, content, etag }, plus what the editor shows. */
  let note = null;
  let notes = [];
  let query = '';
  let mode = 'edit';
  // The note whose images from other sites the reader allowed to load.
  let externalImagesFor = null;
  let dirty = false;
  let saveTimer = null;
  let saving = Promise.resolve();

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
      error.data = data;
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
    node.append(...children.filter((child) => child !== null && child !== undefined));
    return node;
  }

  async function start() {
    const { folder } = await api('GET', '/folder');
    if (folder) {
      await showNotes();
    } else {
      showChoose();
    }
  }

  function showChoose(message) {
    app.replaceChildren(
      el(
        'div',
        { class: 'center' },
        el('h2', {}, t.choose),
        el('p', { class: 'muted' }, message ?? t.chooseHint),
        el('button', { class: 'primary', onclick: chooseFolder }, t.chooseButton),
      ),
    );
  }

  async function chooseFolder() {
    try {
      await rustcloud.requestAccess({ folder: t.folderName, write: true });
      await showNotes();
    } catch {
      showChoose(t.denied);
    }
  }

  async function showNotes() {
    app.replaceChildren(
      el(
        'div',
        { class: 'layout' },
        el(
          'aside',
          { class: 'list' },
          el(
            'header',
            {},
            el('input', { type: 'search', placeholder: t.search, value: query, oninput: onSearch }),
            el('button', { class: 'primary', onclick: createNote }, t.newNote),
          ),
          el('div', { class: 'items', id: 'items' }),
        ),
        el('section', { class: 'editor', id: 'editor' }),
      ),
    );
    await loadList();
    renderEditor();
  }

  let searchTimer = null;
  function onSearch(event) {
    query = event.target.value;
    clearTimeout(searchTimer);
    searchTimer = setTimeout(loadList, 250);
  }

  async function loadList() {
    try {
      notes = await api('GET', '/notes?q=' + encodeURIComponent(query));
    } catch (error) {
      notes = [];
      setStatus(t.failed(error.message));
    }
    renderList();
  }

  function renderList() {
    const items = document.getElementById('items');
    if (!items) return;
    if (!notes.length) {
      items.replaceChildren(el('p', { class: 'group' }, query ? t.nothingFound : t.empty));
      return;
    }
    // Notes of each subfolder together, the top folder first.
    const groups = new Map();
    for (const summary of notes) groups.set(summary.folder, [...(groups.get(summary.folder) ?? []), summary]);
    const ordered = [...groups].sort(([a], [b]) => (a === '' ? -1 : b === '' ? 1 : a.localeCompare(b)));
    const children = ordered.flatMap(([folder, list]) => [
      folder ? el('div', { class: 'group' }, folder) : null,
      ...list.map((summary) =>
        el(
          'button',
          { class: 'item' + (note?.id === summary.id ? ' active' : ''), onclick: () => openNote(summary.id) },
          el('div', { class: 'title' }, summary.title),
          el('div', { class: 'excerpt' }, summary.excerpt || ' '),
        ),
      ),
    ]);
    items.replaceChildren(...children.filter(Boolean));
  }

  async function openNote(id) {
    await flush();
    try {
      note = await api('GET', '/notes/' + id);
      dirty = false;
      mode = 'edit';
      renderEditor();
      renderList();
    } catch (error) {
      setStatus(t.failed(error.message));
    }
  }

  async function createNote() {
    await flush();
    try {
      note = await api('POST', '/notes', { name: t.newName });
      dirty = false;
      mode = 'edit';
      await loadList();
      renderEditor();
      document.querySelector('textarea')?.focus();
    } catch (error) {
      setStatus(t.failed(error.message));
    }
  }

  /** Asks before something is lost; the sandbox blocks the browser's confirm(). */
  function ask(question, yes) {
    return new Promise((resolve) => {
      const answer = (value) => {
        resolve(value);
        dialog.close();
      };
      const dialog = el(
        'dialog',
        {},
        el('p', {}, question),
        el(
          'div',
          { class: 'actions' },
          el('button', { onclick: () => answer(false) }, t.cancel),
          el('button', { class: 'danger', onclick: () => answer(true) }, yes),
        ),
      );
      // Escape closes it without an answer.
      dialog.addEventListener('close', () => {
        resolve(false);
        dialog.remove();
      });
      document.body.append(dialog);
      dialog.showModal();
    });
  }

  async function deleteNote() {
    if (!note || !(await ask(t.confirmDelete(note.title), t.delete))) return;
    clearTimeout(saveTimer);
    try {
      await api('DELETE', '/notes/' + note.id);
      note = null;
      dirty = false;
      await loadList();
      renderEditor();
    } catch (error) {
      setStatus(t.failed(error.message));
    }
  }

  function renderEditor() {
    const editor = document.getElementById('editor');
    if (!editor) return;
    if (!note) {
      editor.replaceChildren(el('p', { class: 'center muted' }, t.pick));
      return;
    }
    const tab = (name, label) =>
      el('button', { class: 'tab' + (mode === name ? ' on' : ''), onclick: () => switchMode(name) }, label);
    editor.replaceChildren(
      el(
        'div',
        { class: 'toolbar' },
        tab('edit', t.edit),
        tab('preview', t.preview),
        el('span', { class: 'status muted', id: 'status' }, dirty ? t.unsaved : t.saved),
        el('button', { class: 'delete', onclick: deleteNote }, t.delete),
      ),
      el('div', { id: 'warning' }),
      mode === 'edit'
        ? el('textarea', { value: note.content, oninput: onInput, spellcheck: true })
        : el('article', { class: 'preview', id: 'preview' }),
    );
    if (mode === 'preview') renderPreview();
  }

  async function switchMode(next) {
    mode = next;
    renderEditor();
  }

  async function renderPreview() {
    // Images from other sites stay links until the reader allows them for
    // this note: loading one tells that site who reads the note and when.
    const external = externalImagesFor === note.id ? '?external_images=1' : '';
    const response = await rustcloud.fetch('/api/preview' + external, { method: 'POST', body: note.content });
    const { html, blocked_images } = await response.json();
    const preview = document.getElementById('preview');
    if (!preview) return;
    // The HTML comes from the app's own renderer, which shows raw HTML as text.
    preview.innerHTML = html;
    if (blocked_images > 0) {
      const show = el('button', { class: 'external-images', onclick: showExternalImages }, t.showExternalImages);
      preview.before(el('p', { class: 'muted small' }, t.externalImagesBlocked(blocked_images), ' ', show));
    }
  }

  function showExternalImages() {
    externalImagesFor = note.id;
    renderEditor();
  }

  function onInput(event) {
    note.content = event.target.value;
    dirty = true;
    setStatus(t.unsaved);
    clearTimeout(saveTimer);
    saveTimer = setTimeout(save, SAVE_DELAY);
  }

  /** Saves now if there are unsaved changes. */
  async function flush() {
    clearTimeout(saveTimer);
    if (dirty) await save();
    await saving;
  }

  function save() {
    if (!note || !dirty) return saving;
    const current = note;
    const content = current.content;
    saving = saving.then(async () => {
      setStatus(t.saving);
      try {
        const saved = await api('PUT', '/notes/' + current.id, { content, etag: current.etag });
        current.etag = saved.etag;
        current.title = saved.title;
        if (current === note && note.content === content) dirty = false;
        setStatus(dirty ? t.unsaved : t.saved);
        loadList();
      } catch (error) {
        if (error.status === 409) {
          showChanged();
        } else {
          setStatus(t.failed(error.message));
        }
      }
    });
    return saving;
  }

  function showChanged() {
    setStatus(t.unsaved);
    const warning = document.getElementById('warning');
    if (!warning) return;
    warning.replaceChildren(
      el(
        'p',
        { class: 'warning' },
        t.changed + ' ',
        el('button', { onclick: () => note && openNoteFresh(note.id) }, t.reload),
      ),
    );
  }

  async function openNoteFresh(id) {
    dirty = false;
    clearTimeout(saveTimer);
    await openNote(id);
  }

  function setStatus(text) {
    const status = document.getElementById('status');
    if (status) status.textContent = text;
  }

  await start();
})();
