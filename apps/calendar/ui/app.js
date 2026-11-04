// The Calendar page: a month grid, the user's calendars and those shared
// with the user, and a form for events. It runs in the panel's sandboxed <iframe>; every request goes
// through rustcloud.fetch. Times come from the app in UTC (or as dates for
// all-day events) and are shown in the browser's time zone.
(async () => {
  const TEXTS = {
    en: {
      title: 'Calendar',
      today: 'Today',
      newEvent: 'New event',
      calendars: 'Calendars',
      newCalendar: 'New calendar',
      connect: 'Connect a device',
      more: (n) => `+${n} more`,
      eventTitle: 'Title',
      untitled: 'New event',
      allDay: 'All day',
      start: 'Starts',
      end: 'Ends',
      repeat: 'Repeat',
      repeats: {
        none: 'Never',
        daily: 'Every day',
        weekly: 'Every week',
        monthly: 'Every month',
        yearly: 'Every year',
        custom: 'As set in another app',
      },
      repeatNote: 'Changes apply to every repeat.',
      calendar: 'Calendar',
      location: 'Location',
      description: 'Notes',
      save: 'Save',
      cancel: 'Cancel',
      delete: 'Delete',
      confirmDelete: (title) => `Delete “${title}”?`,
      confirmDeleteCalendar: (name) => `Delete the calendar “${name}” with all its events?`,
      name: 'Name',
      color: 'Colour',
      changed: 'This event was changed elsewhere. Close the form and open it again.',
      failed: (message) => `Error: ${message}`,
      connectTitle: 'Connect a phone or a desktop calendar',
      connectText:
        'Add a CalDAV account in your calendar app (on iPhone: Settings → Calendar → Accounts → Add Account → Other; on Android: DAVx⁵; Thunderbird: New Calendar → On the Network).',
      connectServer: 'Server',
      connectUser: 'Username',
      connectPassword: 'Password',
      connectPasswordText: 'an app password: create one in Settings → App passwords, with access to Calendar.',
      connectFull: 'If the app asks for the full address',
      close: 'Close',
      share: 'Share…',
      sharedBy: (name) => `shared by ${name}`,
      sharedText: (name, edit) =>
        edit
          ? `${name} shared this calendar with you: you can add and change events.`
          : `${name} shared this calendar with you to view.`,
      readOnly: 'This calendar is shared with you to view only.',
      leave: 'Remove from my calendars',
      confirmLeave: (name) => `Remove “${name}” from your calendars? Its owner keeps it.`,
    },
    ru: {
      title: 'Календарь',
      today: 'Сегодня',
      newEvent: 'Новое событие',
      calendars: 'Календари',
      newCalendar: 'Новый календарь',
      connect: 'Подключить устройство',
      more: (n) => `ещё ${n}`,
      eventTitle: 'Название',
      untitled: 'Новое событие',
      allDay: 'Весь день',
      start: 'Начало',
      end: 'Конец',
      repeat: 'Повтор',
      repeats: {
        none: 'Не повторять',
        daily: 'Каждый день',
        weekly: 'Каждую неделю',
        monthly: 'Каждый месяц',
        yearly: 'Каждый год',
        custom: 'Как задано в другом приложении',
      },
      repeatNote: 'Изменения применятся ко всем повторам.',
      calendar: 'Календарь',
      location: 'Место',
      description: 'Заметки',
      save: 'Сохранить',
      cancel: 'Отмена',
      delete: 'Удалить',
      confirmDelete: (title) => `Удалить «${title}»?`,
      confirmDeleteCalendar: (name) => `Удалить календарь «${name}» со всеми событиями?`,
      name: 'Название',
      color: 'Цвет',
      changed: 'Событие изменили в другом месте. Закройте форму и откройте снова.',
      failed: (message) => `Ошибка: ${message}`,
      connectTitle: 'Подключить телефон или календарь на компьютере',
      connectText:
        'Добавьте в приложении календаря учётную запись CalDAV (iPhone: Настройки → Календарь → Учётные записи → Добавить → Другое; Android: DAVx⁵; Thunderbird: Новый календарь → В сети).',
      connectServer: 'Сервер',
      connectUser: 'Логин',
      connectPassword: 'Пароль',
      connectPasswordText: 'пароль приложения: создайте его в «Настройки → Пароли приложений» с доступом к Календарю.',
      connectFull: 'Если приложение просит полный адрес',
      close: 'Закрыть',
      share: 'Поделиться…',
      sharedBy: (name) => `от ${name}`,
      sharedText: (name, edit) =>
        edit
          ? `${name} поделился с вами этим календарём: вы можете добавлять и менять события.`
          : `${name} поделился с вами этим календарём для просмотра.`,
      readOnly: 'Этим календарём с вами поделились только для просмотра.',
      leave: 'Убрать из моих календарей',
      confirmLeave: (name) => `Убрать «${name}» из ваших календарей? У владельца он останется.`,
    },
  };

  const context = await rustcloud.context;
  const locale = context.locale === 'ru' ? 'ru' : 'en';
  const t = TEXTS[locale];
  document.documentElement.dataset.theme = context.theme;
  document.title = t.title;
  const app = document.getElementById('app');
  const timeZone = Intl.DateTimeFormat().resolvedOptions().timeZone;
  /** Weeks start on Monday, except in English. */
  const firstWeekday = locale === 'en' ? 0 : 1;

  let calendars = [];
  /** Ids of calendars the user hid. */
  const hidden = new Set();
  /** The first day of the month shown. */
  let month = startOfMonth(new Date());
  let occurrences = [];

  async function api(method, path, body) {
    const init = { method };
    if (body !== undefined) {
      init.body = JSON.stringify(body);
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
      else if (key === 'style') node.style.cssText = value;
      else node[key] = value;
    }
    node.append(...children.filter((child) => child !== null && child !== undefined && child !== false));
    return node;
  }

  // --- Dates ----------------------------------------------------------------

  function startOfMonth(date) {
    return new Date(date.getFullYear(), date.getMonth(), 1);
  }

  function addDays(date, days) {
    return new Date(date.getFullYear(), date.getMonth(), date.getDate() + days, date.getHours(), date.getMinutes());
  }

  const pad = (n) => String(n).padStart(2, '0');

  /** `2026-10-29` of a local date. */
  function ymd(date) {
    return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
  }

  /** `2026-10-29T09:00` of a local time, as <input type=datetime-local> wants it. */
  function ymdhm(date) {
    return `${ymd(date)}T${pad(date.getHours())}:${pad(date.getMinutes())}`;
  }

  /** A local date from `2026-10-29`. */
  function parseYmd(text) {
    const [y, m, d] = text.split('-').map(Number);
    return new Date(y, m - 1, d);
  }

  /** The days of the grid: whole weeks around the month. */
  function gridDays() {
    const shift = (month.getDay() - firstWeekday + 7) % 7;
    const first = addDays(month, -shift);
    return Array.from({ length: 42 }, (_, i) => addDays(first, i));
  }

  /** Where an occurrence is: local start and end (exclusive) as Dates. */
  function span(occurrence) {
    if (occurrence.all_day) return [parseYmd(occurrence.start), parseYmd(occurrence.end)];
    // `…Z` is UTC; without it, a floating time, which Date reads as local.
    return [new Date(occurrence.start), new Date(occurrence.end)];
  }

  const timeFormat = new Intl.DateTimeFormat(locale, { hour: '2-digit', minute: '2-digit' });

  // --- Page -----------------------------------------------------------------

  async function start() {
    try {
      calendars = await api('GET', '/calendars');
    } catch (error) {
      app.replaceChildren(el('p', { class: 'center' }, t.failed(error.message)));
      return;
    }
    render();
    await loadMonth();
  }

  function render() {
    const weekdays = Array.from({ length: 7 }, (_, i) =>
      new Intl.DateTimeFormat(locale, { weekday: 'short' }).format(new Date(2024, 0, 7 + firstWeekday + i)),
    );
    app.replaceChildren(
      el(
        'div',
        { class: 'layout' },
        el(
          'aside',
          { class: 'side' },
          el('button', { class: 'primary', onclick: () => openNew(new Date()) }, t.newEvent),
          el('h3', {}, t.calendars),
          el('div', { id: 'calendars' }),
          el('button', { class: 'link', onclick: () => openCalendar(null) }, '+ ' + t.newCalendar),
          el('div', { class: 'grow' }),
          el('button', { class: 'link', onclick: openConnect }, t.connect),
        ),
        el(
          'section',
          { class: 'main' },
          el(
            'header',
            {},
            el('button', { onclick: () => moveMonth(-1), title: '‹' }, '‹'),
            el('button', { onclick: () => moveMonth(1), title: '›' }, '›'),
            el('button', { onclick: () => goToday() }, t.today),
            el('h2', { id: 'month' }),
          ),
          el('div', { class: 'weekdays' }, ...weekdays.map((day) => el('div', {}, day))),
          el('div', { class: 'grid', id: 'grid' }),
          el('p', { class: 'status', id: 'status' }),
        ),
      ),
    );
    renderCalendars();
  }

  function renderCalendars() {
    const list = document.getElementById('calendars');
    list.replaceChildren(
      ...calendars.map((calendar) =>
        el(
          'div',
          { class: 'calendar' },
          el('input', {
            type: 'checkbox',
            checked: !hidden.has(calendar.id),
            style: `accent-color: ${calendar.color}`,
            onchange: (event) => {
              if (event.target.checked) hidden.delete(calendar.id);
              else hidden.add(calendar.id);
              renderGrid();
            },
          }),
          el(
            'button',
            { class: 'link', onclick: () => openCalendar(calendar) },
            calendar.name,
            calendar.owner ? el('span', { class: 'owner' }, t.sharedBy(calendar.owner.display_name)) : null,
          ),
        ),
      ),
    );
  }

  function moveMonth(by) {
    month = new Date(month.getFullYear(), month.getMonth() + by, 1);
    loadMonth();
  }

  function goToday() {
    month = startOfMonth(new Date());
    loadMonth();
  }

  async function loadMonth() {
    document.getElementById('month').textContent = new Intl.DateTimeFormat(locale, {
      month: 'long',
      year: 'numeric',
    }).format(month);
    const days = gridDays();
    const from = days[0].toISOString();
    const to = addDays(days[days.length - 1], 1).toISOString();
    try {
      occurrences = await api('GET', `/events?from=${encodeURIComponent(from)}&to=${encodeURIComponent(to)}`);
      setStatus('');
    } catch (error) {
      occurrences = [];
      setStatus(t.failed(error.message));
    }
    renderGrid();
  }

  function renderGrid() {
    const grid = document.getElementById('grid');
    const today = ymd(new Date());
    const colors = Object.fromEntries(calendars.map((c) => [c.id, c.color]));
    const visible = occurrences
      .filter((o) => !hidden.has(o.calendar))
      .map((o) => ({ ...o, span: span(o) }))
      .sort((a, b) => b.all_day - a.all_day || a.span[0] - b.span[0]);

    grid.replaceChildren(
      ...gridDays().map((day) => {
        const next = addDays(day, 1);
        const events = visible.filter((o) => o.span[0] < next && (o.span[1] > day || +o.span[0] === +day));
        const shown = events.slice(0, 3);
        return el(
          'div',
          {
            class: 'day' + (day.getMonth() === month.getMonth() ? '' : ' other') + (ymd(day) === today ? ' today' : ''),
            onclick: (event) => event.target === event.currentTarget && openNew(day),
          },
          el('span', { class: 'number', onclick: () => openNew(day) }, String(day.getDate())),
          ...shown.map((o) =>
            el(
              'button',
              {
                class: 'event' + (o.all_day ? ' all-day' : ''),
                style: `--color: ${colors[o.calendar] ?? 'gray'}`,
                title: o.summary + (o.location ? ` · ${o.location}` : ''),
                onclick: () => openEvent(o),
              },
              o.all_day ? null : el('span', { class: 'time' }, timeFormat.format(o.span[0])),
              o.summary || t.untitled,
            ),
          ),
          events.length > shown.length
            ? el('span', { class: 'more', onclick: () => openNew(day) }, t.more(events.length - shown.length))
            : null,
        );
      }),
    );
  }

  function setStatus(text) {
    const status = document.getElementById('status');
    if (status) status.textContent = text;
  }

  // --- Event form -------------------------------------------------------------

  function openNew(day) {
    const start = new Date(day.getFullYear(), day.getMonth(), day.getDate(), 9, 0);
    showEventForm({
      calendar: (calendars.find((c) => c.writable && !hidden.has(c.id)) ?? calendars.find((c) => c.writable)).id,
      summary: '',
      location: '',
      description: '',
      all_day: false,
      start: ymdhm(start),
      end: ymdhm(new Date(start.getTime() + 3600_000)),
      repeat: 'none',
    });
  }

  async function openEvent(occurrence) {
    try {
      const event = await api(
        'GET',
        `/events/${encodeURIComponent(occurrence.calendar)}/${encodeURIComponent(occurrence.name)}?tz=${encodeURIComponent(timeZone)}`,
      );
      showEventForm(event);
    } catch (error) {
      setStatus(t.failed(error.message));
    }
  }

  /** The form for `event`; one with a `name` is saved over its file. */
  function showEventForm(event) {
    const isNew = !event.name;
    // Calendars shared to view only: the form just shows the event.
    const readOnly = !isNew && event.writable === false;
    const allDay = el('input', { type: 'checkbox', checked: event.all_day });
    // All-day ends are exclusive in the file and inclusive on the form.
    const endDate = event.all_day ? ymd(addDays(parseYmd(event.end), -1)) : event.end;
    const startInput = el('input', {
      type: event.all_day ? 'date' : 'datetime-local',
      value: event.start,
      required: true,
    });
    const endInput = el('input', { type: event.all_day ? 'date' : 'datetime-local', value: endDate, required: true });
    allDay.addEventListener('change', () => {
      const [start, end] = [startInput.value, endInput.value];
      for (const input of [startInput, endInput]) input.type = allDay.checked ? 'date' : 'datetime-local';
      startInput.value = allDay.checked ? start.slice(0, 10) : `${start}T09:00`;
      endInput.value = allDay.checked ? end.slice(0, 10) : `${end}T10:00`;
    });
    const summary = el('input', { value: event.summary, placeholder: t.untitled });
    const repeatOptions = ['none', 'daily', 'weekly', 'monthly', 'yearly'];
    if (event.repeat === 'custom') repeatOptions.push('custom');
    const repeat = el(
      'select',
      {},
      ...repeatOptions.map((value) => el('option', { value, selected: value === event.repeat }, t.repeats[value])),
    );
    const calendar = el(
      'select',
      { disabled: !isNew },
      ...calendars
        .filter((c) => !isNew || c.writable)
        .map((c) => el('option', { value: c.id, selected: c.id === event.calendar }, c.name)),
    );
    const location = el('input', { value: event.location });
    const description = el('textarea', { rows: 3, value: event.description });
    const message = el('p', { class: 'error' });

    const dialog = el(
      'dialog',
      {},
      el(
        'form',
        {
          method: 'dialog',
          onsubmit: async (e) => {
            e.preventDefault();
            const form = {
              summary: summary.value,
              location: location.value,
              description: description.value,
              all_day: allDay.checked,
              start: startInput.value,
              end: allDay.checked ? ymd(addDays(parseYmd(endInput.value), 1)) : endInput.value,
              tz: timeZone,
              repeat: repeat.value,
              etag: event.etag,
            };
            try {
              if (isNew) await api('POST', `/events/${encodeURIComponent(calendar.value)}`, form);
              else
                await api(
                  'PUT',
                  `/events/${encodeURIComponent(event.calendar)}/${encodeURIComponent(event.name)}`,
                  form,
                );
              dialog.close();
              await loadMonth();
            } catch (error) {
              message.textContent = error.status === 409 ? t.changed : t.failed(error.message);
            }
          },
        },
        el('label', {}, t.eventTitle, summary),
        el('label', { class: 'check' }, allDay, t.allDay),
        el('div', { class: 'row' }, el('label', {}, t.start, startInput), el('label', {}, t.end, endInput)),
        el('div', { class: 'row' }, el('label', {}, t.repeat, repeat), el('label', {}, t.calendar, calendar)),
        event.repeat !== 'none' && !isNew && !readOnly ? el('p', { class: 'muted small' }, t.repeatNote) : null,
        readOnly ? el('p', { class: 'muted small' }, t.readOnly) : null,
        el('label', {}, t.location, location),
        el('label', {}, t.description, description),
        message,
        el(
          'div',
          { class: 'actions' },
          isNew || readOnly
            ? null
            : el(
                'button',
                {
                  type: 'button',
                  class: 'danger',
                  onclick: async () => {
                    if (!(await ask(t.confirmDelete(event.summary || t.untitled), t.delete))) return;
                    try {
                      await api(
                        'DELETE',
                        `/events/${encodeURIComponent(event.calendar)}/${encodeURIComponent(event.name)}`,
                      );
                      dialog.close();
                      await loadMonth();
                    } catch (error) {
                      message.textContent = t.failed(error.message);
                    }
                  },
                },
                t.delete,
              ),
          el('div', { class: 'grow' }),
          el('button', { type: 'button', onclick: () => dialog.close() }, readOnly ? t.close : t.cancel),
          readOnly ? null : el('button', { type: 'submit', class: 'primary' }, t.save),
        ),
      ),
    );
    if (readOnly) {
      for (const field of dialog.querySelectorAll('input, select, textarea')) field.disabled = true;
    }
    showDialog(dialog);
    summary.focus();
  }

  function showDialog(dialog) {
    dialog.addEventListener('close', () => dialog.remove());
    document.body.append(dialog);
    dialog.showModal();
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
        { class: 'ask' },
        el('p', {}, question),
        el(
          'div',
          { class: 'actions' },
          el('div', { class: 'grow' }),
          el('button', { onclick: () => answer(false) }, t.cancel),
          el('button', { class: 'danger', onclick: () => answer(true) }, yes),
        ),
      );
      // Escape closes it without an answer.
      dialog.addEventListener('close', () => resolve(false));
      showDialog(dialog);
    });
  }

  // --- Calendars ----------------------------------------------------------------

  /** The form for a calendar; `null` creates one. */
  function openCalendar(calendar) {
    if (calendar?.owner) {
      openShared(calendar);
      return;
    }
    const own = calendars.filter((c) => !c.owner);
    const name = el('input', { value: calendar?.name ?? '', required: true });
    const color = el('input', { type: 'color', value: (calendar?.color ?? '#3b82f6').slice(0, 7) });
    const message = el('p', { class: 'error' });
    const dialog = el(
      'dialog',
      {},
      el(
        'form',
        {
          method: 'dialog',
          onsubmit: async (e) => {
            e.preventDefault();
            const body = { name: name.value, color: color.value };
            try {
              if (calendar) await api('PUT', `/calendars/${encodeURIComponent(calendar.id)}`, body);
              else await api('POST', '/calendars', body);
              calendars = await api('GET', '/calendars');
              dialog.close();
              renderCalendars();
              await loadMonth();
            } catch (error) {
              message.textContent = t.failed(error.message);
            }
          },
        },
        el('label', {}, t.name, name),
        el('label', {}, t.color, color),
        message,
        el(
          'div',
          { class: 'actions' },
          calendar && own.length > 1
            ? el(
                'button',
                {
                  type: 'button',
                  class: 'danger',
                  onclick: async () => {
                    if (!(await ask(t.confirmDeleteCalendar(calendar.name), t.delete))) return;
                    try {
                      await api('DELETE', `/calendars/${encodeURIComponent(calendar.id)}`);
                      calendars = await api('GET', '/calendars');
                      dialog.close();
                      renderCalendars();
                      await loadMonth();
                    } catch (error) {
                      message.textContent = t.failed(error.message);
                    }
                  },
                },
                t.delete,
              )
            : null,
          calendar
            ? el(
                'button',
                {
                  type: 'button',
                  // The panel asks whom to share with; others then see it in their Calendar.
                  onclick: () => rustcloud.share({ path: `calendars/${calendar.id}`, label: calendar.name }),
                },
                t.share,
              )
            : null,
          el('div', { class: 'grow' }),
          el('button', { type: 'button', onclick: () => dialog.close() }, t.cancel),
          el('button', { type: 'submit', class: 'primary' }, t.save),
        ),
      ),
    );
    showDialog(dialog);
    name.focus();
  }

  /** What a calendar someone shared is, and a way to give it up. */
  function openShared(calendar) {
    const message = el('p', { class: 'error' });
    const dialog = el(
      'dialog',
      {},
      el('h3', {}, calendar.name),
      el('p', {}, t.sharedText(calendar.owner.display_name, calendar.writable)),
      message,
      el(
        'div',
        { class: 'actions' },
        el(
          'button',
          {
            class: 'danger',
            onclick: async () => {
              if (!(await ask(t.confirmLeave(calendar.name), t.leave))) return;
              try {
                await api('DELETE', `/calendars/${encodeURIComponent(calendar.id)}`);
                calendars = await api('GET', '/calendars');
                dialog.close();
                renderCalendars();
                await loadMonth();
              } catch (error) {
                message.textContent = t.failed(error.message);
              }
            },
          },
          t.leave,
        ),
        el('div', { class: 'grow' }),
        el('button', { onclick: () => dialog.close() }, t.close),
      ),
    );
    showDialog(dialog);
  }

  // --- Connecting devices -----------------------------------------------------

  async function openConnect() {
    let info = { path: '/app/calendar/dav/', username: context.user?.username ?? '' };
    try {
      info = await api('GET', '/connect');
    } catch {
      // The defaults above are right for this server too.
    }
    // The page's own address is the cloud's, even in the sandbox.
    const server = `${location.protocol}//${location.host}`;
    const code = (text) => el('code', {}, text);
    const dialog = el(
      'dialog',
      {},
      el(
        'div',
        { class: 'connect' },
        el('h3', {}, t.connectTitle),
        el('p', {}, t.connectText),
        el(
          'dl',
          {},
          el('dt', {}, t.connectServer),
          el('dd', {}, code(server)),
          el('dt', {}, t.connectUser),
          el('dd', {}, code(info.username)),
          el('dt', {}, t.connectPassword),
          el('dd', {}, t.connectPasswordText),
        ),
        el('p', { class: 'muted small' }, `${t.connectFull}: `, code(server + info.path)),
        el(
          'div',
          { class: 'actions' },
          el('div', { class: 'grow' }),
          el('button', { onclick: () => dialog.close() }, t.close),
        ),
      ),
    );
    showDialog(dialog);
  }

  start();
})();
