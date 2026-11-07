# WebDAV

`/dav/` — корневая папка пользователя по WebDAV (RFC 4918, классы 1 и 2). Подходит для файловых
менеджеров ОС (Проводник Windows, Finder, GNOME, KDE), davfs2, rclone, мобильных приложений.
Код — [files/src/dav](../../crates/files/src/dav/mod.rs).

## Вход

- **Basic** с именем пользователя и [паролем приложения](app-passwords.md) с правом на `files`:
  `write` — всё, `read` — только `GET`, `HEAD`, `OPTIONS`, `PROPFIND` (иначе `403 READ_ONLY`).
  Пароль приложения без `files` → `403 APP_PASSWORD_SCOPE`.
- **Bearer** — токен клиентского приложения ([вход приложений](auth.md#вход-приложений)).
- Cookie сессии веб-интерфейса **не** принимается. Без входа или с неверным паролем → `401`
  с `WWW-Authenticate: Basic`.
- `OPTIONS` отвечает без входа: `DAV: 1, 2`, `Allow: …`.

```sh
rclone config create cloud webdav url=https://cloud.example.com/dav/ vendor=owncloud \
  user=alice pass=$(rclone obscure "$APP_PASSWORD")
curl -u alice:$APP_PASSWORD -X PROPFIND -H 'Depth: 1' https://cloud.example.com/dav/
```

Windows по умолчанию не отправляет Basic по `http://` — подключайте по `https://`.

## Методы

| Метод | Что делает |
|-------|------------|
| `PROPFIND` | свойства файла или папки; `Depth: 0` или `1` (`infinity` и без заголовка — как `1`) |
| `GET`, `HEAD` | содержимое файла, `Range`, `If-None-Match`, `ETag`, `Last-Modified` |
| `PUT` | новый файл (`201`) или новое содержимое (`204`); `If-Match`, `If-None-Match: *` → `412` |
| `MKCOL` | папка; уже есть → `405`, нет папки выше → `409` |
| `DELETE` | в корзину, как в интерфейсе |
| `COPY`, `MOVE` | в `Destination` (полный URL или путь под `/dav/`); `Overwrite: F` и место занято → `412`; заменённое уходит в корзину |
| `PROPPATCH` | только время изменения файла, остальное → `403` (всё или ничего, RFC 4918) |
| `LOCK`, `UNLOCK` | блокировки для совместимости, см. ниже |

**Свойства** (`allprop`): `resourcetype`, `displayname`, `getcontentlength`, `getcontenttype`,
`getetag` (только у файлов), `getlastmodified` (время изменения, которое дал клиент — см.
[mtime](files.md)), `creationdate`, `supportedlock`, `lockdiscovery`. По запросу — квота
(RFC 4331): `quota-used-bytes`, `quota-available-bytes` (меньшее из остатка квоты и места на диске).

**Время изменения** клиент задаёт:
- заголовком `X-OC-Mtime: <секунды Unix>` в `PUT` (rclone, клиенты Nextcloud), ответ —
  `X-OC-MTime: accepted`;
- `PROPPATCH` свойства `getlastmodified` или `Win32LastModifiedTime` (Windows). Остальные
  `Win32*` принимаются и не сохраняются.

**Блокировки.** Finder и Windows пишут только на сервер класса 2, поэтому `LOCK` выдаёт токен
(`Lock-Token`, `Timeout: Second-3600`), но сервер блокировку **не соблюдает**: файл может изменить
кто угодно. `LOCK` на свободное имя создаёт пустой файл (`201`). Блокировки приложений
(редакторов) соблюдаются: запись в такой файл → `423 Locked`.

## Отличия от полного WebDAV

- Корзина (`.trash`) не видна и недоступна; создать в корне `.trash` нельзя (`403`).
- Скрытые файлы и папки (`.name`, в том числе `.appdata`) видны — как в ОС.
- Ошибки — обычный JSON ([errors.md](errors.md)); статусы — по RFC 4918: `412` вместо
  `409 FILE_CHANGED`, `507` вместо `413` при нехватке квоты или места, `423` для заблокированных
  файлов, `502` для `Destination` на другом сервере.
