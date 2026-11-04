# Журнал и `audit!`

Макрос — [core/src/audit.rs](../../crates/core/src/audit.rs). Запись в базу —
[server/src/journal](../../crates/server/src/journal/mod.rs). Как выглядит для администратора —
[Журнал](../admin.md#журнал).

## Записать событие

```rust
use rustcloud_core::audit;

audit!("file.download", name = file.name, size = file.size, "file downloaded");
audit!(warn, "auth.login_failed", user = login, "failed login");
```

- Первый аргумент — код события `<объект>.<действие>`. `warn` перед ним делает событие
  предупреждением.
- Поля попадают в `details` (JSON). Поля `user` и `ip` заменяют взятые из запроса.
- Последний аргумент — сообщение (английский, для логов).

Пользователя и IP передавать не нужно: они берутся из span запроса, который заполняют
`network::guard` и экстрактор `CurrentUser`.

## Что ещё попадает в журнал

Любое `tracing::warn!` и `tracing::error!` из любого крейта — с `action: null`. Например,
`Error::internal` пишет `error!`, поэтому каждый ответ `500` виден в журнале.

## Как это работает

```text
audit! / warn! / error!
  → JournalLayer (tracing)        — добавляет user и ip из span, кладёт в очередь
  → ограниченная очередь           — полна? событие считается в dropped, запрос не ждёт
  → фоновая задача Journal        — пишет пачками в log_entries, удаляет старше keep_days
```

- [layer.rs](../../crates/server/src/journal/layer.rs) — слой `tracing`.
- [mod.rs](../../crates/server/src/journal/mod.rs) — запись, очистка, чтение (`Journal::entries`).
- Ошибки самой записи идут с target `rustcloud_journal` и в журнал не попадают.

## Чек-лист нового события

1. `audit!("…")` в сервисе модуля, после успешного действия.
2. Текст для UI — в `journalActions` в [web/src/lib/i18n.ts](../../web/src/lib/i18n.ts), en и ru.
   Без него показывается `message`.
3. Строка в таблице событий в [admin.md](../admin.md#журнал).
4. Тест: события пишутся в журнал в [server/tests/api.rs](../../crates/server/tests/api.rs).

См. также: [`GET /api/v1/admin/journal`](../api/admin.md#журнал)
