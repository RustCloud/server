# Раздел настроек

Трейт `Settings` и `Live<T>` — [core/src/settings.rs](../../crates/core/src/settings.rs).
Один раздел = одна таблица в [`rustcloud.toml`](../configuration.md) = одна вкладка в
[Администрировании](../admin.md#настройки). Сервер и веб для нового раздела менять не нужно:
форма строится по `fields()`.

## Пример

```rust
use rustcloud_core::{Error, Field, Result, Settings};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FilesSettings {
    /// Largest file that can be uploaded, in MiB; `None` is unlimited.
    pub max_upload_mib: Option<u64>,
}

impl Settings for FilesSettings {
    const SECTION: &'static str = "files";

    fn fields() -> Vec<Field> {
        vec![Field::optional_number("max_upload_mib", "MiB")]
    }

    fn validate(&self) -> Result<()> {
        if self.max_upload_mib == Some(0) {
            return Err(Error::bad_request("INVALID_SETTINGS", "max_upload_mib must be positive"));
        }
        Ok(())
    }
}
```

Полностью — [files/src/settings.rs](../../crates/files/src/settings.rs).

## Поля

| Конструктор                         | В UI                       | Тип в Rust      |
|-------------------------------------|----------------------------|-----------------|
| `Field::bool(key)`                  | переключатель              | `bool`          |
| `Field::number(key, unit)`          | число                      | `u32`, `u64`, … |
| `Field::optional_number(key, unit)` | число, пусто — без лимита  | `Option<u64>`   |
| `Field::text(key)`                  | строка                     | `String`        |
| `Field::list(key)`                  | строки, по одной в строке  | `Vec<String>`   |

`#[serde(default, deny_unknown_fields)]` обязателен: неполная таблица работает, опечатка — ошибка.

## Подключение

В [server/src/lib.rs](../../crates/server/src/lib.rs), `AppState::new`:

```rust
let files = Arc::new(Live::new(config.section::<FilesSettings>()?));
let sections: Vec<Arc<dyn SettingsSection>> = vec![/* … */, files.clone()];
```

Модуль хранит `Arc<Live<FilesSettings>>` и читает текущие значения:

```rust
let limit = self.settings.read(FilesSettings::max_upload_bytes);
```

После сохранения в UI `Live` обновляется на месте — перезапуск не нужен.

## Чек-лист

1. Подписи полей — `sections` в [web/src/lib/i18n.ts](../../web/src/lib/i18n.ts), en и ru.
2. Пример с комментариями — [rustcloud.example.toml](../../rustcloud.example.toml).
3. Таблица — [configuration.md](../configuration.md) и README.

См. также: [`/api/v1/admin/settings`](../api/admin.md#настройки)
