# Пароли приложений

Отдельный пароль для программы, которая входит по HTTP Basic (CalDAV, CardDAV, WebDAV-клиенты),
чтобы она не получала пароль от аккаунта. У каждого пароля свои права — какие приложения, чтение
или запись — и отзывается он отдельно, как токены доступа в Git-хостингах. Правила —
[auth/src/app_passwords.rs](../../crates/auth/src/app_passwords.rs), маршруты —
[server/src/app_passwords.rs](../../crates/server/src/app_passwords.rs). В интерфейсе —
«Настройки → Пароли приложений».

## Вход

```sh
curl -u alice:$APP_PASSWORD http://localhost:8080/app/calendar/
```

- Открывает только [маршруты плагинов](apps.md#маршруты-плагинов) `/app/<id>/` из своих прав;
  API `/api/v1` и веб-интерфейс — нет.
- Чужое приложение → `403 APP_PASSWORD_SCOPE`.
- `read` пропускает только `GET`, `HEAD`, `OPTIONS`, `PROPFIND`, `REPORT`, остальное →
  `403 READ_ONLY`. Плагин, отвечая на такой запрос, тоже ничего не может изменить.
- Неверные имя или пароль → `401` с `WWW-Authenticate: Basic`, неудачи считаются вместе
  с обычными входами ([`accounts.max_login_failures`](../configuration.md#accounts)).
- Отключённый пользователь не входит и с паролем приложения.

Пароль — 43 случайных символа, в базе только SHA-256. Показывается один раз — в ответе на создание.

## Объект `AppPassword`

```json
{
  "id": "…", "name": "Phone",
  "scopes": [{"app": "calendar", "access": "write"}],
  "created_at": "…", "last_used_at": "…"
}
```

`access` — `read` или `write`. `last_used_at` обновляется не чаще раза в 10 минут, `null` —
ещё не использовался.

## Запросы

| Запрос                                | Тело              | Ответ                                  |
|---------------------------------------|-------------------|----------------------------------------|
| `GET /api/v1/app-passwords`           | —                 | `[AppPassword]`, новые первыми         |
| `POST /api/v1/app-passwords`          | `{name, scopes}`  | `201 AppPassword` + `"password": "…"`  |
| `DELETE /api/v1/app-passwords/{id}`   | —                 | `204`, пароль перестаёт работать сразу |

Ошибки: `400 INVALID_NAME` (пустое или длиннее 100 символов), `400 INVALID_SCOPES` (нет приложений
или одно дважды), `400 TOO_MANY_APP_PASSWORDS` (больше 50), `404 APP_NOT_FOUND`,
`404 APP_PASSWORD_NOT_FOUND`. Создание и отзыв попадают в [журнал](../admin.md#журнал)
(`app_password.create`, `app_password.revoke`).
