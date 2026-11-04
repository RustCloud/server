# Публичные ссылки

Ссылка даёт доступ к файлу или папке без входа. Код — [shares.rs](../../crates/files/src/shares.rs).
В веб-интерфейсе ссылка выглядит как `<public_url>/#/s/<token>`.

## Объект `Share`

```json
{"id": "…", "token": "…", "expires_at": null, "created_at": "…", "node": {…}}
```

`node` — [`Node`](files.md#объект-node).

## Управление (с авторизацией)

| Запрос                         | Тело                         | Ответ        |
|--------------------------------|------------------------------|--------------|
| `GET /api/v1/shares`           | —                            | `[Share]` — ссылки пользователя |
| `POST /api/v1/shares`          | `{node_id, expires_at?}`     | `201 Share`  |
| `DELETE /api/v1/shares/{id}`   | —                            | `204`        |

Ошибки: `400 INVALID_EXPIRY` (срок в прошлом), `400 ROOT_FOLDER` (корень не публикуется).

## Открытие (без авторизации)

`GET /api/v1/public/{token}[?node=<id>]`

```json
{"expires_at": null, "shared": {…}, "current": {…}, "path": […], "children": […]}
```

- `shared` — опубликованный узел, `current` — открытый сейчас (`?node=` — узел внутри
  опубликованной папки).
- `path` — папки между `shared` и `current`; `children` — содержимое `current`, если это папка.

`GET /api/v1/public/{token}/content[?node=<id>][&download=1]` — скачивание, те же `Range`
и `ETag`, что у [файлов](files.md#скачивание).

Истёкшая или удалённая ссылка — `404 SHARE_NOT_FOUND`. Скачивания попадают в
[журнал](../admin.md#журнал) как `share.download`.
