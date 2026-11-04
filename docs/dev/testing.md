# Тесты и CI

## Rust

```sh
cargo test --workspace                                  # SQLite во временной папке
RUSTCLOUD_TEST_POSTGRES=postgres://postgres@localhost/postgres \
  cargo test --workspace                                # PostgreSQL, своя БД на каждый тест
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

| Где                                                      | Что проверяет                     |
|----------------------------------------------------------|-----------------------------------|
| `#[cfg(test)]` в модулях                                 | отдельные функции                 |
| [auth/tests/service.rs](../../crates/auth/tests/service.rs)   | сервис аккаунтов и сессий    |
| [files/tests/service.rs](../../crates/files/tests/service.rs) | сервис файлов и ссылок       |
| [db/tests/schema.rs](../../crates/db/tests/schema.rs)    | миграции на обеих СУБД            |
| [server/tests/api.rs](../../crates/server/tests/api.rs)  | HTTP API целиком, `TestApp`       |

Тестовая база — [db/src/testing.rs](../../crates/db/src/testing.rs).

## Веб

```sh
cd web
npm run dev     # :5173, API проксируется на :8080
npm run check   # svelte-check
npm test        # vitest, файлы *.test.ts рядом с кодом
```

## Браузерный смоук-тест

[web/e2e/smoke.mjs](../../web/e2e/smoke.mjs) — Playwright проходит основные сценарии против
свежего сервера и сохраняет скриншоты. Команда запуска — в начале файла.

## CI

- [test.yml](../../.github/workflows/test.yml) — fmt, clippy, тесты на SQLite и PostgreSQL,
  проверка и сборка веба.
- [lint.yml](../../.github/workflows/lint.yml) — MegaLinter и SBOM.
- [release.yml](../../.github/workflows/release.yml) — вручную или по тегу `v*`: сборка и
  [публикация образа](../install.md#готовый-образ).
