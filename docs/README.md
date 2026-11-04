# Документация RustCloud

Self-hosted облако для файлов на Rust. Обзор и быстрый старт — в [README проекта](../README.md).

## Для администратора

- [Установка](install.md) — Docker, Docker Compose, сборка из исходников, reverse proxy
- [Настройки](configuration.md) — файл `rustcloud.toml`, переменные окружения, все параметры
- [Администрирование](admin.md) — пользователи, настройки в UI, хранилище, журнал

## Приложения

- [Заметки](apps/notes.md) — Markdown-заметки в папке ваших файлов
- [Календарь](apps/calendar.md) — события, синхронизация с телефоном по CalDAV

## API

- [Обзор](api/README.md) — аутентификация, CSRF, формат ошибок
- [Аккаунты и вход](api/auth.md) — `/api/v1/auth`, `/api/v1/admin/users`
- [Файлы](api/files.md) — `/api/v1/files`, `/api/v1/usage`
- [Публичные ссылки](api/shares.md) — `/api/v1/shares`, `/api/v1/public`
- [Приложения](api/apps.md) — `/api/v1/apps`, манифесты
- [Доступ приложений](api/grants.md) — `/api/v1/grants`
- [Пароли приложений](api/app-passwords.md) — `/api/v1/app-passwords`, вход CalDAV-клиентов
- [Общие данные приложений](api/app-shares.md) — `/api/v1/app-shares`, общие календари
- [Администрирование](api/admin.md) — настройки, хранилище, журнал
- [Коды ошибок](api/errors.md)

## Для разработчика

- [Устройство](dev/architecture.md) — крейты, модули, трейты-стратегии
- [Раздел настроек](dev/settings.md) — трейт `Settings`, как добавить свой
- [Журнал и `audit!`](dev/audit.md) — как записать событие
- [Хранилище](dev/blob-store.md) — трейт `BlobStore`, как подключить S3 и др.
- [Плагины (WASM)](dev/plugins.md) — контракт WIT, песочница, свой плагин
- [Способы входа](dev/login-providers.md) — `CredentialProvider`, LDAP, OIDC
- [Тесты и CI](dev/testing.md)

Планы и открытые вопросы — [ROADMAP.md](../ROADMAP.md).
