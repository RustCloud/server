# Способы входа (`CredentialProvider`)

Трейт — [auth/src/provider.rs](../../crates/auth/src/provider.rs), сервис —
[auth/src/service.rs](../../crates/auth/src/service.rs).

Способ входа проверяет учётные данные и отвечает `Identity { provider, subject }`: «способ X
ручается, что это аккаунт Y». Превращение `Identity` в пользователя и сессию одинаково для всех —
`Auth::sign_in`. Связь хранится в таблице `identities(provider, subject)`, поэтому новые способы
не меняют схему.

## Логин + секрет (пароль, LDAP)

```rust
#[async_trait]
impl CredentialProvider for LdapProvider {
    fn name(&self) -> &str { "ldap" }

    async fn verify(&self, login: &str, secret: &str) -> Result<Option<Identity>> {
        // неверные данные — Ok(None), не ошибка
    }
}

let auth = Auth::new(db, accounts).with_provider(Arc::new(LdapProvider::new(…)));
```

Клиент выбирает способ полем `provider` в [`POST /api/v1/auth/login`](../api/auth.md#вход);
список доступных — в [`GET /api/v1/auth/status`](../api/auth.md#состояние).
Ограничение попыток и журнал (`auth.login`, `auth.login_failed`) работают для любого способа.

Пример — `PasswordProvider` в том же файле: Argon2id, `subject` — имя пользователя в нижнем регистре.

## Редирект (OIDC)

Не укладывается в «проверь логин и секрет». Поток живёт в
[server/src/oidc.rs](../../crates/server/src/oidc.rs) (`openidconnect`, код с PKCE): callback
проверяет ID-токен, строит `Identity { provider: "oidc:corp", subject: sub }` и вызывает
`Auth::sign_in_external` ([auth/src/external.rs](../../crates/auth/src/external.rs)). Тот находит
аккаунт по личности, а при первом входе — по подтверждённой почте или создаёт новый, применяет
решение сервиса о правах администратора и выдаёт сессию. Так же сможет работать любой вход
через внешний сервис (SAML, другой протокол): ему нужно только собрать `ExternalLogin`.

Сервисы хранятся в таблице `oidc_providers` (секрет клиента — только там, API его не отдаёт).
Поведение и API — [Аккаунты](../api/auth.md#вход-через-сервисы-oidc).

См. также: [Устройство](architecture.md), [Аккаунты](../api/auth.md)
