# Аккаунты и вход

Модуль [auth](../../crates/auth/src/lib.rs), маршруты — [http.rs](../../crates/auth/src/http.rs).

## Объект `User`

```json
{
  "id": "…", "username": "alice", "display_name": "Alice", "email": null,
  "is_admin": false, "disabled": false, "quota_bytes": null, "created_at": "…"
}
```

## Состояние

`GET /api/v1/auth/status` — без авторизации.

```json
{"needs_setup": false, "registration_open": false, "providers": ["password"]}
```

`needs_setup: true` — аккаунтов ещё нет, первая регистрация создаст администратора. Его можно
создать и при запуске, переменными `RUSTCLOUD_ADMIN_USERNAME` и `RUSTCLOUD_ADMIN_PASSWORD`
([Первый запуск](../install.md#первый-запуск)).

## Регистрация

`POST /api/v1/auth/register` → `201`, ответ как у [входа](#вход).

```json
{"username": "alice", "password": "…", "display_name": "Alice", "email": "a@example.com"}
```

Работает, если `needs_setup` или [`accounts.open_registration`](../configuration.md#accounts),
иначе `403 REGISTRATION_CLOSED`.

## Вход

`POST /api/v1/auth/login`

```json
{"username": "alice", "password": "…", "provider": "password"}
```

`provider` необязателен. Ответ ставит cookie и возвращает токен — он показывается один раз:

```json
{"token": "…", "expires_at": "…", "user": {…}}
```

Токен получают только программы. Запросу со страницы браузера (с заголовком `Origin` или
`Sec-Fetch-Site`, кроме `none`) токен не приходит, только cookie: скрипт на странице не сможет
унести сессию.

Ошибки: `401 INVALID_CREDENTIALS` (и для отключённого аккаунта: ответ не выдаёт, верен ли
пароль), `429 TOO_MANY_ATTEMPTS`: после трёх неудач подряд аккаунт ждёт перед следующей попыткой 1 с, 2 с,
4 с … до минуты, а адрес, превысивший [`max_login_failures`](../configuration.md#accounts) ×5,
блокируется до конца 15 минут. Попытка считается до проверки пароля, поэтому пачка
одновременных попыток тоже упирается в предел;
`429 SERVER_BUSY` — все ядра заняты проверкой паролей, попробуйте через секунду.

## Выход

`POST /api/v1/auth/logout` → `204`. Завершает текущую сессию.

`POST /api/v1/auth/logout-others` → `{"ended": 2}`. Завершает все сессии пользователя, кроме
текущей («выйти на других устройствах»). Пароли приложений остаются.

## Профиль

| Запрос                          | Тело                                      | Ответ  |
|---------------------------------|-------------------------------------------|--------|
| `GET /api/v1/auth/me`           | —                                         | `User` |
| `PATCH /api/v1/auth/me`         | `{display_name?, email?}`; `""` удаляет email | `User` |
| `POST /api/v1/auth/password`    | `{current_password, new_password, revoke_app_passwords?}` | `204`  |

Смена пароля завершает все остальные сессии пользователя; с `revoke_app_passwords: true` отзывает
и все [пароли приложений](app-passwords.md).

## Вход через сервисы (OIDC)

Сервисы OpenID Connect (Keycloak, Authentik, Google…) добавляет администратор
([ниже](#сервисы-входа-админ)). Модуль — [oidc.rs](../../crates/server/src/oidc.rs).

| Запрос                                   | Кто          | Ответ |
|------------------------------------------|--------------|-------|
| `GET /api/v1/auth/oidc`                  | все          | `[{id, name}]` — включённые сервисы, кнопки страницы входа |
| `GET /api/v1/auth/oidc/{id}/login`       | браузер      | `303` на сервис |
| `POST /api/v1/auth/oidc/{id}/link`       | пользователь | `{url}` — куда отправить браузер, чтобы привязать сервис к своему аккаунту |
| `GET /api/v1/auth/oidc/{id}/callback`    | сервис       | `303` на `/` (вход), `/#/settings?oidc_linked={id}` (привязка) или страницу с `?oidc_error=<код>` |

Поток — authorization code с PKCE. Начало запоминает на сервере `state`, `nonce` и PKCE на
10 минут, а `state` кладёт в cookie `rustcloud_oidc` (HttpOnly, SameSite=Lax, путь
`/api/v1/auth/oidc`). Callback принимается только от того же браузера: чужая ссылка с кодом
никого не впустит. ID-токен проверяется полностью (подпись по ключам сервиса, `iss`, `aud`,
`nonce`, срок); `sub` из него — личность `("oidc:{id}", sub)` в таблице `identities`.
Группы, почта и имя берутся из ID-токена, а если их там нет — из userinfo.

Адрес возврата — `{public_url}/api/v1/auth/oidc/{id}/callback`; без
[`server.public_url`](../configuration.md#server) — адрес, по которому открыт сервер. Если
`public_url` задан, а браузер пришёл по другому адресу, вход сначала переходит на `public_url`.

**Первый вход** того, кого сервер ещё не знает:

1. С включённым `link_by_email` и почтой, которую сервис отметил подтверждённой
   (`email_verified: true`), — аккаунт с той же почтой, если он ровно один.
2. Иначе с `auto_create` — новый аккаунт: имя из `preferred_username`, почты или имени,
   приведённое к правилам имён; занятое получает `-2`, `-3`… Пароля у аккаунта нет.
3. Иначе — `NO_ACCOUNT`.

**Группы** (поле `groups_claim`, точки — вложенные объекты: `realm_access.roles`):
с непустым `user_groups` входят только участники `user_groups` или `admin_groups`
(`NOT_ALLOWED`); с непустым `admin_groups` при каждом входе участники становятся
администраторами, остальные теряют права. Последнего администратора сервер так не понижает.

Коды в `oidc_error`: `OIDC_UNAVAILABLE` (сервис выключен или недоступен), `OIDC_EXPIRED`
(вход начат в другом браузере или дольше 10 минут), `OIDC_CANCELLED` (сервис ответил ошибкой,
например пользователь отказался), `OIDC_FAILED` (ответ сервиса не прошёл проверку —
подробности в журнале), `NO_ACCOUNT`, `NOT_ALLOWED`, `ACCOUNT_DISABLED`, `LOGIN_TAKEN`.
Неудачи пишутся в журнал как `auth.login_failed` с `method=oidc:{id}` и причиной.

## Способы входа аккаунта

| Запрос                              | Ответ |
|-------------------------------------|-------|
| `GET /api/v1/auth/logins`           | `[{id, provider, created_at}]`, `provider` — `password` или `oidc:{id}` |
| `DELETE /api/v1/auth/logins/{id}`   | `204` — отвязать сервис |

Пароль так не убрать (`400 CANNOT_UNLINK_PASSWORD`), последний способ входа тоже
(`400 LAST_LOGIN`). Привязка — [`POST /api/v1/auth/oidc/{id}/link`](#вход-через-сервисы-oidc);
сервис, уже привязанный к другому аккаунту, — `409 LOGIN_TAKEN`.

## Сервисы входа (админ)

| Запрос                            | Тело       | Ответ |
|-----------------------------------|------------|-------|
| `GET /api/v1/admin/oidc`          | —          | `[Provider]` |
| `POST /api/v1/admin/oidc`         | `Provider` | `201 Provider` |
| `PUT /api/v1/admin/oidc/{id}`     | `Provider` | `Provider` |
| `DELETE /api/v1/admin/oidc/{id}`  | —          | `204` |

```json
{
  "id": "corp", "name": "Аккаунт компании", "issuer": "https://auth.example.com/realms/main",
  "client_id": "rustcloud", "client_secret": "…", "scopes": "openid profile email",
  "enabled": true, "auto_create": true, "link_by_email": false,
  "groups_claim": "groups", "user_groups": ["staff"], "admin_groups": ["cloud-admins"]
}
```

В ответе вместо `client_secret` — `has_secret`, и ещё `created_at` и `redirect_url` (адрес,
который нужно указать на сервисе). При изменении пустой `client_secret` оставляет прежний;
`id` и `issuer` не меняются (`400 INVALID_OIDC_PROVIDER`): тот же `sub` от другого сервиса —
другой человек. При добавлении сервер читает `{issuer}/.well-known/openid-configuration`
(`400 OIDC_DISCOVERY_FAILED` с причиной). Удаление стирает привязки аккаунтов к сервису;
аккаунты остаются. Значения по умолчанию: `scopes` — `openid profile email` (`openid`
добавляется всегда), `groups_claim` — `groups`, `enabled` и `auto_create` — `true`.

## Пользователи (админ)

| Запрос                               | Тело                                                         | Ответ        |
|--------------------------------------|--------------------------------------------------------------|--------------|
| `GET /api/v1/admin/users`            | —                                                            | `[User]`     |
| `POST /api/v1/admin/users`           | `{username, password, display_name?, email?, is_admin?}`     | `201 User`   |
| `PATCH /api/v1/admin/users/{id}`     | `{display_name?, is_admin?, disabled?, quota_bytes?, password?}` | `User`   |

- `quota_bytes: null` снимает квоту.
- `disabled: true` или новый `password` завершают все сессии пользователя.
- Себя нельзя отключить или лишить прав — `400 CANNOT_CHANGE_SELF`.

См. также: [Способы входа](../dev/login-providers.md)
