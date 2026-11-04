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
