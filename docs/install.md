# Установка

## Готовый образ

Образ публикуется в GitHub Container Registry: вручную (Actions → Release → Run workflow) или
тегом `v*` ([release.yml](../.github/workflows/release.yml)). Тесты и линтер он не запускает —
перед запуском убедитесь, что Test и Lint для этого коммита зелёные.

Теги образа `ghcr.io/rustcloud/server`:

| Тег                 | Что это                                      |
|---------------------|----------------------------------------------|
| `:latest`           | последний релиз                              |
| `:sha-<короткий sha>` | этот коммит — чтобы закрепить или откатиться |
| `:<версия>`         | релиз по тегу `v<версия>`                    |

```sh
docker run -d -p 8080:8080 -v rustcloud-data:/data ghcr.io/rustcloud/server:latest
```

## Docker (SQLite)

```sh
docker build -t rustcloud .
docker run -d -p 8080:8080 -v rustcloud-data:/data --name rustcloud rustcloud
```

Всё хранится в томе `/data`: база `rustcloud.db`, файлы `files/`, настройки `rustcloud.toml`.
Образ — [Dockerfile](../Dockerfile).

## Docker Compose (PostgreSQL)

```sh
echo "POSTGRES_PASSWORD=$(openssl rand -hex 16)" > .env
docker compose up -d
```

- Образ — [последний релиз](#готовый-образ) `:latest`; `:<sha>` — конкретный релиз, `build: .` — собрать на месте.
- Настройки — переменные `RUSTCLOUD_*` в `environment` сервиса `rustcloud` (примеры там закомментированы),
  см. [Настройки](configuration.md#откуда-берутся-значения).
- В `.env` — только `POSTGRES_PASSWORD`, compose подставляет его и в базу, и в `RUSTCLOUD_DATABASE_URL`.
- Слушает только `127.0.0.1:8080` — рассчитан на reverse proxy. Подробности в начале
  [compose.yml](../compose.yml).

## Из исходников

Нужны Rust 1.97+ и Node.js 22+.

```sh
(cd web && npm ci && npm run build)   # веб встраивается в бинарник
cargo build --release
./target/release/rustcloud            # данные — в ./data
./target/release/rustcloud --config /etc/rustcloud.toml
```

## Первый запуск

Откройте `http://<сервер>:8080` и создайте аккаунт — первый становится администратором.
Дальше пользователей создаёт администратор ([Администрирование](admin.md#пользователи)) или
включается открытая регистрация ([`accounts.open_registration`](configuration.md#accounts)).

## За reverse proxy

TLS RustCloud сам не терминирует. За nginx или Caddy задайте:

```toml
[server]
public_url = "https://cloud.example.com"   # Secure-cookie и адрес публичных ссылок
trusted_proxies = ["127.0.0.1"]            # чей X-Forwarded-For / -Proto принимать
```

Прокси не должен ограничивать размер тела запроса и буферизовать загрузки
(nginx: `client_max_body_size 0; proxy_request_buffering off;`).

## Проверка

`GET /health` → `{"status":"ok","version":"…"}`, если сервер и база работают. Его же использует
`HEALTHCHECK` в образе.

## Обновление

До беты формат данных может меняться — сделайте копию `/data` (и базы PostgreSQL).
Миграции применяются при запуске ([crates/db/src/migrations.rs](../crates/db/src/migrations.rs)).

См. также: [Настройки](configuration.md)
