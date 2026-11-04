# Устройство

```text
crates/
  core/           Error, BlobStore, Settings, audit!, валидация      — общее для всех
  plugin-api/     Manifest, версия API и WIT-контракт плагинов       — контракт приложений
  wasm-host/      запуск WASM-плагинов в песочнице (wasmtime)
  db/             сущности SeaORM и миграции (SQLite / PostgreSQL)
  storage-local/  BlobStore: файлы в локальной папке
  auth/           аккаунты, способы входа, сессии
  files/          дерево файлов, загрузка/скачивание, квоты, ссылки
  server/         бинарник: конфиг, сборка модулей, журнал, сеть, встроенный веб
web/              интерфейс на Svelte 5, встраивается в бинарник
```

| Крейт            | Точка входа                                                        |
|------------------|--------------------------------------------------------------------|
| `rustcloud-core` | [core/src/lib.rs](../../crates/core/src/lib.rs)                    |
| `rustcloud-plugin-api` | [plugin-api/src/lib.rs](../../crates/plugin-api/src/lib.rs)  |
| `rustcloud-wasm-host` | [wasm-host/src/lib.rs](../../crates/wasm-host/src/lib.rs) |
| `rustcloud-db`   | [entities.rs](../../crates/db/src/entities.rs), [migrations.rs](../../crates/db/src/migrations.rs) |
| `rustcloud-auth` | [auth/src/lib.rs](../../crates/auth/src/lib.rs)                    |
| `rustcloud-files`| [files/src/lib.rs](../../crates/files/src/lib.rs)                  |
| `rustcloud-storage-local` | [storage-local/src/lib.rs](../../crates/storage-local/src/lib.rs) |
| `rustcloud` (server) | [server/src/lib.rs](../../crates/server/src/lib.rs), [main.rs](../../crates/server/src/main.rs) |

## Принципы

1. **Трейты-стратегии на точках расширения** — `CredentialProvider`, `BlobStore`: новая реализация
   не требует переписывать остальное.
2. **Модули приносят свои маршруты** — сервер их только объединяет.
3. **Одно место для каждого правила** — вход: `Auth::sign_in`, права на файлы: `Files::check_access`,
   ошибки: `rustcloud_core::Error`.
4. **Безопасность по умолчанию** — Argon2id, отзываемые сессии, CSRF-заголовок, лимит попыток входа,
   пользовательский контент не исполняется на нашем домене, строгий CSP.
5. **Админ не работает с чужими файлами** — управляет аккаунтами и сервером, но не открывает,
   не скачивает и не даёт другим доступ к файлам пользователей.
6. **Простой код и тесты на поведение** — сервисы на SQLite и PostgreSQL, HTTP-сценарии сквозными
   тестами, UI смоук-тестом.

## Принятые решения

| Вопрос              | Решение                                                 | Почему                                                                 |
|---------------------|---------------------------------------------------------|------------------------------------------------------------------------|
| БД                  | SeaORM 2, PostgreSQL **или** SQLite по URL              | SQLite — дома без настройки, PostgreSQL — для многих пользователей      |
| Сессии              | случайный токен, в БД — SHA-256                         | JWT не нужен: сессия всё равно проверяется в БД, а так отзывается сразу |
| Способы входа       | `identities(provider, subject, secret_hash)`            | пароль, OIDC, LDAP — строки одной таблицы, у пользователя может быть несколько |
| Корень пользователя | папка с `parent_id = NULL`, частичный уникальный индекс | уникальность имён работает без особых случаев для корня                 |
| Содержимое файлов   | блоб по новому UUID на каждую версию                    | замена файла не ломает идущее скачивание; запись атомарная             |
| Фронтенд            | Svelte 5, встроен в бинарник                            | один файл для установки; Node нужен только при сборке                  |

Что планируется и что обсуждается — [ROADMAP.md](../../ROADMAP.md).

## Модуль

Модуль (`auth`, `files`, будущие приложения) приносит:

- **манифест** — `Manifest` из [plugin-api](../../crates/plugin-api/src/manifest.rs): id, версии,
  название, `provides`, пункт в панели. Сервер собирает манифесты в
  [`GET /api/v1/apps`](../api/apps.md), веб строит по нему боковую панель;

- **сервис** — `Arc<Files>`, `Arc<Auth>`: вся логика, без HTTP;
- **`router()`** — свои маршруты; `server` только объединяет их в `app()`;
- **раздел настроек** — [`Settings`](settings.md);
- **события журнала** — [`audit!`](audit.md).

Подключение — в [server/src/lib.rs](../../crates/server/src/lib.rs): манифест в `Apps::builtin`
([apps.rs](../../crates/server/src/apps.rs)), поле в `AppState`
(обработчики берут его через `State<Arc<…>>` благодаря `FromRef`), раздел в списке `sections`,
`.merge(module::router())` в `app()`.

## Трейты-стратегии

| Трейт                                     | Что заменяет                  | Реализации       |
|-------------------------------------------|-------------------------------|------------------|
| [`BlobStore`](blob-store.md)              | где лежат байты файлов        | локальный диск   |
| [`CredentialProvider`](login-providers.md)| как пользователь входит       | пароль           |
| [`Settings`](settings.md)                 | раздел настроек модуля        | `[server]`, `[accounts]`, `[files]`, `[journal]` |

## Запрос

```text
TraceLayer → заголовки безопасности → network::guard → CORS → маршрут
                                       │
                                       └ ClientInfo (IP с учётом trusted_proxies, https),
                                         span "client" — его читает журнал
```

- Пользователь: экстрактор `CurrentUser` / `AdminUser` ([auth/src/http.rs](../../crates/auth/src/http.rs)),
  он же записывает имя в span запроса.
- Клиент: экстрактор `ClientInfo` ([core/src/client.rs](../../crates/core/src/client.rs)).
- Ошибки: любой обработчик возвращает `rustcloud_core::Result`, `Error` сам превращается в
  [JSON-ответ](../api/errors.md).
- Права на файлы: только `Files::check_access` ([files/src/service.rs](../../crates/files/src/service.rs)).

## База

Таблицы: `users`, `identities`, `sessions`, `nodes`, `shares`, `log_entries`
([entities.rs](../../crates/db/src/entities.rs)). Миграции — только добавлять новые, старые не
менять ([migrations.rs](../../crates/db/src/migrations.rs)). Схема проверяется на обеих СУБД
([db/tests/schema.rs](../../crates/db/tests/schema.rs)).

## Веб

[web/src](../../web/src): `pages/` — страницы, `components/` — части, `lib/api.ts` — клиент API,
`lib/i18n.ts` — тексты (en, ru). Хеш-роутер: `#/files/<id>`, `#/s/<token>`.
Сборка `web/dist` встраивается в бинарник ([server/src/web.rs](../../crates/server/src/web.rs)).
