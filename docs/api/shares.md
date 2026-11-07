# Публичные ссылки

Ссылка даёт доступ к файлу или папке без входа. Код — [shares.rs](../../crates/files/src/shares.rs).
В веб-интерфейсе ссылка выглядит как `<public_url>/#/s/<token>`.

## Объект `Share`

```json
{"id": "…", "token": "…", "expires_at": null, "created_at": "…", "node": {…},
 "has_password": false, "allow_upload": false}
```

`node` — [`Node`](files.md#объект-node). `has_password` — нужен пароль; `allow_upload` — посетители
могут загружать файлы в папку.

## Управление (с авторизацией)

| Запрос                         | Тело                         | Ответ        |
|--------------------------------|------------------------------|--------------|
| `GET /api/v1/shares`           | —                            | `[Share]` — ссылки пользователя |
| `POST /api/v1/shares`          | `{node_id, expires_at?, password?, allow_upload?}` | `201 Share`  |
| `PATCH /api/v1/shares/{id}`    | `{expires_at?, password?, allow_upload?}` | `Share` |
| `DELETE /api/v1/shares/{id}`   | —                            | `204`        |

- В `PATCH` отсутствующее поле не меняется; `expires_at: null` — бессрочно, `password: ""` —
  без пароля. Новый пароль закрывает ссылку для всех, кто открыл её со старым.
- Пароль — от 4 символов, хранится хешем Argon2. `allow_upload` — только для папок.

Ошибки: `400 INVALID_EXPIRY` (срок в прошлом), `400 ROOT_FOLDER` (корень не публикуется),
`400 WEAK_LINK_PASSWORD`, `400 NOT_A_FOLDER` (загрузка в файл).

## Пароль

Ссылка с паролем на всё (просмотр, скачивание, архив, загрузку) отвечает
`403 PASSWORD_REQUIRED`. Посетитель вводит пароль:

`POST /api/v1/public/{token}/unlock` `{password}` → `204` и cookie `rustcloud_link`
(HttpOnly, SameSite=Lax, 30 дней) только на путь `/api/v1/public/{token}` — так работают и
обычные ссылки на скачивание. В cookie — HMAC id ссылки по хешу пароля: сменили пароль —
cookie больше не подходит.

- Неверный пароль — `403 WRONG_LINK_PASSWORD`. После 10 неверных за 15 минут каждая следующая
  попытка ждёт (1 с, 2 с, 4 с … до минуты) — `429 TOO_MANY_ATTEMPTS`; ссылку при этом не
  блокирует. Неверные пароли — в [журнале](../admin.md#журнал) (`share.wrong_password`).

## Загрузка посетителями

`POST /api/v1/public/{token}/upload?name=<имя>[&node=<папка>]`, тело — файл → `201 Node`.

- Только если у ссылки `allow_upload`, в опубликованную папку или папку внутри неё
  (`403 UPLOAD_NOT_ALLOWED` иначе). В `GET …/public/{token}` это видно по `can_upload`.
- Ничего не перезаписывается: занятое имя получает номер — `отчёт (2).pdf`.
- Файл идёт в квоту владельца, с его лимитами размера; в журнале — `share.upload`.
- Загруженное видят все, у кого есть ссылка (как и остальное содержимое папки).

## Открытие (без авторизации)

`GET /api/v1/public/{token}[?node=<id>]`

```json
{"expires_at": null, "shared": {…}, "current": {…}, "path": […], "children": […], "can_upload": false}
```

- `shared` — опубликованный узел, `current` — открытый сейчас (`?node=` — узел внутри
  опубликованной папки).
- `path` — папки между `shared` и `current`; `children` — содержимое `current`, если это папка.

`GET /api/v1/public/{token}/content[?node=<id>][&download=1]` — скачивание, те же `Range`
и `ETag`, что у [файлов](files.md#скачивание).

`GET /api/v1/public/{token}/zip[?node=<id>]` — опубликованная папка или папка внутри неё
[архивом](files.md#папка-архивом).

Истёкшая или удалённая ссылка — `404 SHARE_NOT_FOUND`. Скачивания попадают в
[журнал](../admin.md#журнал) как `share.download`.
