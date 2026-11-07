# API

JSON API под `/api/v1`. Маршруты собирает [server/src/lib.rs](../../crates/server/src/lib.rs),
каждый модуль приносит свой `router()`.

| Раздел                         | Пути                                              |
|--------------------------------|---------------------------------------------------|
| [Аккаунты и вход](auth.md)     | `/auth/*`, `/admin/users`                         |
| [Файлы](files.md)              | `/files/*` (и [лента изменений](files.md#лента-изменений)), `/usage` |
| [Публичные ссылки](shares.md)  | `/shares`, `/public/{token}`                      |
| [Приложения](apps.md)          | `/apps`                                           |
| [Доступ приложений](grants.md) | `/grants`                                         |
| [Пароли приложений](app-passwords.md) | `/app-passwords`                          |
| [Общие данные приложений](app-shares.md) | `/app-shares`                          |
| [Администрирование](admin.md)  | `/admin/settings`, `/admin/storage`, `/admin/journal` |
| Служебное                      | `GET /api/v1/info`, `GET /health`                 |

## Аутентификация

Получите токен через [`POST /api/v1/auth/login`](auth.md#вход) и передавайте его:

```sh
curl -H "Authorization: Bearer $TOKEN" http://localhost:8080/api/v1/files/root
```

Клиентские приложения получают короткоживущий access-токен через браузер — см.
[Вход приложений](auth.md#вход-приложений). Браузер вместо этого получает HttpOnly-cookie `rustcloud_session`. Программы вроде CalDAV-клиентов
входят в плагины по Basic с [паролем приложения](app-passwords.md).

## CSRF

Запросы **с cookie**, меняющие данные (`POST`, `PUT`, `PATCH`, `DELETE`), обязаны содержать
заголовок `X-Requested-With` (любое значение), иначе `403 CSRF_CHECK_FAILED`.
С `Authorization: Bearer` заголовок не нужен. Код: [auth/src/http.rs](../../crates/auth/src/http.rs).

## Ошибки

```json
{"error": {"code": "FILE_NOT_FOUND", "message": "File not found"}}
```

`code` стабилен — сравнивайте по нему, `message` — для человека. Все коды: [errors.md](errors.md).

## Типы

- `id` — UUID, время — RFC 3339 в UTC, размеры — в байтах.
- Отсутствующее поле в `PATCH` — «не менять».

## Служебное

`GET /api/v1/info` → `{"version": "…", "public_url": "…"}` — без авторизации.

`GET /health` → `{"status": "ok", "version": "…"}`, `500`, если база недоступна.
