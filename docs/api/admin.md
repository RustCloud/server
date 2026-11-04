# Администрирование

Все запросы требуют прав администратора, иначе `403 FORBIDDEN`.
Аккаунты — в [auth.md](auth.md#пользователи-админ).

## Настройки

`GET /api/v1/admin/settings` → список разделов:

```json
[{
  "name": "files",
  "fields": [
    {"key": "max_upload_mib", "kind": "number", "unit": "MiB", "optional": true},
    {"key": "trash_days", "kind": "number", "unit": "days", "optional": false},
    {"key": "versions_keep", "kind": "number", "unit": "versions", "optional": false},
    {"key": "versions_days", "kind": "number", "unit": "days", "optional": false}
  ],
  "values": {"max_upload_mib": null, "trash_days": 30, "versions_keep": 10, "versions_days": 30},
  "locked": []
}]
```

- `kind`: `bool`, `number` (`unit`, `optional`), `text`, `list`. По ним UI рисует форму.
- `locked` — ключи из переменных окружения.

`PUT /api/v1/admin/settings/{section}` — тело: объект значений раздела. Недостающие ключи
получают значения по умолчанию. Применяется сразу и пишется в файл.

Ошибки: `400 INVALID_SETTINGS`, `404 SETTINGS_NOT_FOUND`.
Параметры: [Настройки](../configuration.md) · Код: [settings_file.rs](../../crates/server/src/settings_file.rs)

## Плагины

`GET /api/v1/admin/plugins` → все плагины, первым — `files`:

```json
[{
  "id": "calendar", "source": "shipped", "manifest": {…},
  "status": {"state": "running"}, "enabled": true,
  "limits": {"memory": 4, "time": 1, "workers": 2},
  "effective": {"scale": {"memory": 4, "time": 1, "workers": 2}, "memory_mib": 256, "data_mib": 64,
                "time_seconds": 10, "wall_seconds": 60, "jobs_mib": 1024},
  "queue": {"exif": {"waiting": 1200, "failed": 2}, "faces": {"waiting": 4800, "failed": 0}},
  "storage": {"files": 1, "bytes": 174000000},
  "download": {"state": "running", "path": "models/faces.onnx", "done": 52000000, "total": 174000000, "left": 1}
}]
```

- `source`: `built_in` (часть сервера), `shipped` (поставляется с сервером), `installed`.
- `status.state`: `running`, `disabled`, `failed` — тогда в `status.error` причина.
  `manifest` — `null`, если манифест не прочитался.
- `limits` — [множители](../dev/plugins.md#лимиты), которые выбрал администратор; `null` — как в
  манифесте. `effective` — что плагин получает (с учётом `[plugins] max_memory`) и что из этого
  следует для одного вызова; `jobs_mib` — сколько памяти его фоновые задачи займут одновременно
  в худшем случае (`workers` порций самого большого вида).
- `queue` — сколько задач каждого вида [фоновых задач](../dev/plugins.md#фоновые-задачи) осталось
  (вместе с выполняемыми) и сколько с ошибкой, по всем пользователям.
- `storage` — сколько лежит в [хранилище плагина](../dev/plugins.md#хранилище-плагина).
- `download` — загрузка файлов из [`[[downloads]]`](apps.md#загрузки) манифеста: `running` (файл `path`,
  `done` из `total` байт, `left` файлов вместе с этим) или `failed` (`path`, `error`). Нет поля — всё
  скачано или скачивать нечего. Загрузка начинается, когда плагин работает (включён), и
  останавливается, если его отключили.

`PUT /api/v1/admin/plugins/{id}` — тело `{enabled, limits?}`, ответ — плагин. `limits` —
`{memory, time, workers}` или `null` (как в манифесте); `memory` и `time` — 1, 2, 4, 8, 16, 32, 64,
`memory` не больше `[plugins] max_memory`, `workers` — 1–16; иначе `400 INVALID_SETTINGS`.
Применяется сразу.

`POST /api/v1/admin/plugins` — тело: zip-архив (до 64 МиБ) с `manifest.toml` и `app.wasm`
в корне или в одной папке. Ответ `201` — плагин. Новый — `disabled`; обновление с тем же `id`
остаётся включённым, только если просит у пользователей и у сервера (`[limits]`) не больше прежнего.

`GET /api/v1/admin/use/ffmpeg` → состояние [`use.ffmpeg`](../dev/use.md): `{available, ffmpeg, layers: {rlimits, no_new_privs, seccomp, landlock}, problem, capabilities}`. `problem` — почему недоступен или что слабее, чем должно быть (нет Landlock).

`GET /api/v1/admin/use/onnx` → состояние [`use.onnx`](../dev/use.md#use-onnx): `{available, engine, layers, problem, keep_loaded_seconds, loaded: [{app_id, path, idle_seconds, busy}]}` — `loaded` — модели в памяти сейчас, недавние первыми.

`DELETE /api/v1/admin/plugins/{id}` → `204`, только `source: installed`. Хранилище плагина удаляется
вместе с ним, данные пользователей (AppData) остаются.

`GET /api/v1/admin/plugins/{id}/storage` → файлы хранилища `[{path, size, updated_at}]` по пути.
`DELETE /api/v1/admin/plugins/{id}/storage` — очищает хранилище, ответ — плагин; файлы из
`[[downloads]]` скачиваются заново. `POST /api/v1/admin/plugins/{id}/downloads` — повторить
загрузку после ошибки, ответ — плагин.

`PUT /api/v1/admin/plugins/{id}/settings` — тело: значения [настроек сервера](apps.md#настройки-сервера)
плагина `{ключ: значение}`, ответ — плагин. Отсутствующий или `null` ключ очищает настройку;
у `secret` отсутствующий или пустой ключ оставляет сохранённое значение, `null` — очищает.
В ответе и в списке у плагина `settings` — значения без секретов, `secrets_set` — какие секреты заданы.

Ошибки: `404 PLUGIN_NOT_FOUND`, `403 PLUGIN_REQUIRED` (`files` не отключается),
`409 PLUGIN_CANT_RUN` (включить плагин, который не может работать), `400 INVALID_SETTINGS`,
`400 INVALID_PLUGIN` (не архив, нет файлов, неверный манифест или компонент — причина в `message`),
`409 PLUGIN_EXISTS` (такой `id` у плагина, который поставляется с сервером),
`403 PLUGIN_NOT_REMOVABLE`. Неизвестный ключ или значение не того вида — `400 INVALID_SETTINGS`.
Код: [plugins.rs](../../crates/server/src/plugins.rs)

## Сервер

`GET /api/v1/admin/system` — машина и процесс сервера для вкладки «Сервер»:

```json
{
  "version": "0.1.0-alpha.1", "revision": "e9588eb…", "uptime_seconds": 3600,
  "os": "Alpine Linux v3.22", "kernel": "6.12.0", "arch": "x86_64", "container": true,
  "cpus": 6, "load": [0.52, 0.41, 0.3],
  "memory": {"total_bytes": 0, "available_bytes": 0, "swap_total_bytes": 0, "swap_free_bytes": 0},
  "container_memory": {"limit_bytes": 0, "used_bytes": 0},
  "process": {"memory_bytes": 0, "threads": 17},
  "database": {"kind": "postgres", "version": "17.6", "size_bytes": 0}
}
```

Читается из `/proc` и `/sys`; где их нет (не Linux) — `null`. `revision` — коммит сборки
(`docker build --build-arg REVISION=…`), `container_memory` — только при лимите памяти cgroup v2.
Код: [system.rs](../../crates/server/src/system.rs)

## Хранилище

`GET /api/v1/admin/storage`

```json
{
  "kind": "local", "location": "/data/files",
  "capacity": {"total_bytes": 0, "free_bytes": 0},
  "used_percent": 81.2, "nearly_full": true,
  "stored_bytes": 0, "file_count": 0,
  "speed_test": {"bytes": 67108864, "write_speed": 92.7, "read_speed": 1168.3, "tested_at": "…"}
}
```

`capacity` и `used_percent` — `null` у хранилищ без фиксированного размера (S3).
Скорость — МиБ/с; `speed_test: null`, если тест не запускался с момента старта.

`POST /api/v1/admin/storage/speed-test` → объект `speed_test` нового теста. Не запустится,
если свободно меньше двух его размеров — `409 NOT_ENOUGH_SPACE`.

Код: [storage.rs](../../crates/files/src/storage.rs)

## Журнал

`GET /api/v1/admin/journal?problems=true&q=alice&before=<id>`

| Параметр   | Что делает                                          |
|------------|-----------------------------------------------------|
| `problems` | только `WARN` и `ERROR`                             |
| `q`        | текст в сообщении, событии, пользователе, адресе, деталях |
| `before`   | записи старше этого `id` — следующая страница       |

Ответ — до 100 записей, новые первыми:

```json
{
  "entries": [{
    "id": 42, "time": "…", "level": "INFO", "action": "file.download",
    "message": "file downloaded", "username": "alice", "ip": "192.0.2.1",
    "details": {"name": "report.pdf", "size": 52133}
  }],
  "dropped": 0
}
```

`action: null` — обычное предупреждение или ошибка сервера. `dropped` — сколько событий
потеряно с запуска. Список событий: [Журнал](../admin.md#журнал).
Код: [journal/mod.rs](../../crates/server/src/journal/mod.rs)
