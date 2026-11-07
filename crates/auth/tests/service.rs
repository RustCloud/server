//! Behaviour of the auth service against a real database.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

use rustcloud_auth::{
    Access, AccountSettings, AccountUpdate, Auth, AuthorizeRequest, NewAccount, NewAppPassword, ProfileUpdate, Scope,
};
use rustcloud_core::Live;
use rustcloud_db::testing::{test_db, TestDb};

async fn auth_with(settings: AccountSettings) -> (TestDb, Auth) {
    let db = test_db().await;
    let auth = Auth::new(db.conn.clone(), Arc::new(Live::new(settings)));
    (db, auth)
}

async fn auth() -> (TestDb, Auth) {
    auth_with(AccountSettings::default()).await
}

fn account(username: &str) -> NewAccount {
    NewAccount { username: username.into(), password: "password123".into(), display_name: None, email: None }
}

#[tokio::test]
async fn first_registered_user_becomes_admin_and_registration_closes() {
    let (_db, auth) = auth().await;
    assert!(auth.status().await.unwrap().needs_setup);

    let admin = auth.register(account("Admin")).await.unwrap();
    assert!(admin.is_admin);
    assert_eq!(admin.username, "admin", "usernames are lower-cased");
    assert_eq!(admin.display_name, "admin", "display name defaults to the username");

    let status = auth.status().await.unwrap();
    assert!(!status.needs_setup);
    assert!(!status.registration_open);
    let err = auth.register(account("bob")).await.unwrap_err();
    assert_eq!(err.code, "REGISTRATION_CLOSED");
}

#[tokio::test]
async fn the_first_admin_from_the_environment_is_made_only_once() {
    let (_db, auth) = auth().await;
    let admin = auth.create_first_admin(account("boss")).await.unwrap().unwrap();
    assert!(admin.is_admin);
    assert!(!auth.status().await.unwrap().needs_setup, "the setup page is closed");
    assert!(auth.create_first_admin(account("other")).await.unwrap().is_none(), "accounts exist");
}

#[tokio::test]
async fn open_registration_creates_regular_users() {
    let (_db, auth) = auth_with(AccountSettings { open_registration: true, ..Default::default() }).await;
    auth.register(account("admin")).await.unwrap();
    let bob = auth.register(account("bob")).await.unwrap();
    assert!(!bob.is_admin);
}

#[tokio::test]
async fn usernames_are_unique_regardless_of_case() {
    let (_db, auth) = auth().await;
    auth.create_user(account("alice"), false).await.unwrap();
    let err = auth.create_user(account("ALICE"), false).await.unwrap_err();
    assert_eq!(err.code, "USERNAME_TAKEN");
}

#[tokio::test]
async fn login_creates_a_session_that_resolves_to_the_user() {
    let (_db, auth) = auth().await;
    let alice = auth.create_user(account("alice"), false).await.unwrap();

    let session = auth.login("password", "Alice", "password123", None).await.unwrap();
    assert_eq!(session.user.id, alice.id);

    let resolved = auth.session_user(&session.token).await.unwrap().unwrap();
    assert_eq!(resolved.user.id, alice.id);
    assert!(auth.session_user("not-a-token").await.unwrap().is_none());
}

#[tokio::test]
async fn wrong_password_and_unknown_user_look_the_same() {
    let (_db, auth) = auth().await;
    auth.create_user(account("alice"), false).await.unwrap();

    let wrong_password = auth.login("password", "alice", "nope-nope", None).await.unwrap_err();
    let unknown_user = auth.login("password", "mallory", "nope-nope", None).await.unwrap_err();
    assert_eq!(wrong_password.code, "INVALID_CREDENTIALS");
    assert_eq!(wrong_password, unknown_user);
}

#[tokio::test]
async fn logout_ends_the_session() {
    let (_db, auth) = auth().await;
    auth.create_user(account("alice"), false).await.unwrap();
    let session = auth.login("password", "alice", "password123", None).await.unwrap();
    let resolved = auth.session_user(&session.token).await.unwrap().unwrap();

    auth.logout(resolved.session_id).await.unwrap();
    assert!(auth.session_user(&session.token).await.unwrap().is_none());
}

#[tokio::test]
async fn disabled_users_cannot_log_in_and_lose_their_sessions() {
    let (_db, auth) = auth().await;
    let admin = auth.create_user(account("admin"), true).await.unwrap();
    let alice = auth.create_user(account("alice"), false).await.unwrap();
    let session = auth.login("password", "alice", "password123", None).await.unwrap();

    let disable = AccountUpdate { disabled: Some(true), ..Default::default() };
    auth.update_account(&admin, alice.id, disable).await.unwrap();

    assert!(auth.session_user(&session.token).await.unwrap().is_none());
    let err = auth.login("password", "alice", "password123", None).await.unwrap_err();
    assert_eq!(err.code, "INVALID_CREDENTIALS", "like a wrong password: no hint the password was right");
}

#[tokio::test]
async fn admins_cannot_lock_themselves_out() {
    let (_db, auth) = auth().await;
    let admin = auth.create_user(account("admin"), true).await.unwrap();
    for update in [
        AccountUpdate { disabled: Some(true), ..Default::default() },
        AccountUpdate { is_admin: Some(false), ..Default::default() },
    ] {
        let err = auth.update_account(&admin, admin.id, update).await.unwrap_err();
        assert_eq!(err.code, "CANNOT_CHANGE_SELF");
    }
}

#[tokio::test]
async fn changing_the_password_keeps_only_the_current_session() {
    let (_db, auth) = auth().await;
    auth.create_user(account("alice"), false).await.unwrap();
    let laptop = auth.login("password", "alice", "password123", None).await.unwrap();
    let phone = auth.login("password", "alice", "password123", None).await.unwrap();
    let laptop_session = auth.session_user(&laptop.token).await.unwrap().unwrap();

    let err = auth.change_password(&laptop_session, "wrong-current", "new-password-1", false).await.unwrap_err();
    assert_eq!(err.code, "WRONG_PASSWORD");

    auth.change_password(&laptop_session, "password123", "new-password-1", false).await.unwrap();
    assert!(auth.session_user(&laptop.token).await.unwrap().is_some());
    assert!(auth.session_user(&phone.token).await.unwrap().is_none());
    assert!(auth.login("password", "alice", "password123", None).await.is_err());
    assert!(auth.login("password", "alice", "new-password-1", None).await.is_ok());
}

#[tokio::test]
async fn app_passwords_are_revoked_with_the_password_only_when_asked() {
    let (_db, auth) = auth().await;
    let alice = auth.create_user(account("alice"), false).await.unwrap();
    let kept = auth.create_app_password(&alice, calendar_password(Access::Read)).await.unwrap();
    let laptop = auth.login("password", "alice", "password123", None).await.unwrap();
    let session = auth.session_user(&laptop.token).await.unwrap().unwrap();

    auth.change_password(&session, "password123", "new-password-1", false).await.unwrap();
    assert!(auth.app_password_user("alice", &kept.password, None).await.unwrap().is_some());
    auth.change_password(&session, "new-password-1", "new-password-2", true).await.unwrap();
    assert!(auth.app_password_user("alice", &kept.password, None).await.unwrap().is_none());
}

#[tokio::test]
async fn logging_out_on_other_devices_keeps_this_one() {
    let (_db, auth) = auth().await;
    auth.create_user(account("alice"), false).await.unwrap();
    let laptop = auth.login("password", "alice", "password123", None).await.unwrap();
    let phone = auth.login("password", "alice", "password123", None).await.unwrap();
    let tablet = auth.login("password", "alice", "password123", None).await.unwrap();
    let session = auth.session_user(&laptop.token).await.unwrap().unwrap();

    assert_eq!(auth.logout_others(&session).await.unwrap(), 2);
    assert!(auth.session_user(&laptop.token).await.unwrap().is_some());
    assert!(auth.session_user(&phone.token).await.unwrap().is_none());
    assert!(auth.session_user(&tablet.token).await.unwrap().is_none());
}

#[tokio::test]
async fn admin_can_reset_a_password_and_set_a_quota() {
    let (_db, auth) = auth().await;
    let admin = auth.create_user(account("admin"), true).await.unwrap();
    let alice = auth.create_user(account("alice"), false).await.unwrap();

    let update =
        AccountUpdate { password: Some("reset-by-admin".into()), quota_bytes: Some(Some(1024)), ..Default::default() };
    let updated = auth.update_account(&admin, alice.id, update).await.unwrap();
    assert_eq!(updated.quota_bytes, Some(1024));
    assert!(auth.login("password", "alice", "reset-by-admin", None).await.is_ok());

    let remove_quota = AccountUpdate { quota_bytes: Some(None), ..Default::default() };
    let updated = auth.update_account(&admin, alice.id, remove_quota).await.unwrap();
    assert_eq!(updated.quota_bytes, None);
}

#[tokio::test]
async fn profile_update_validates_and_clears_email() {
    let (_db, auth) = auth().await;
    let alice = auth.create_user(account("alice"), false).await.unwrap();

    let bad = ProfileUpdate { email: Some("not-an-email".into()), ..Default::default() };
    assert_eq!(auth.update_profile(alice.id, bad).await.unwrap_err().code, "INVALID_EMAIL");

    let set = ProfileUpdate { display_name: Some("  Alice Liddell ".into()), email: Some("alice@example.com".into()) };
    let updated = auth.update_profile(alice.id, set).await.unwrap();
    assert_eq!(updated.display_name, "Alice Liddell");
    assert_eq!(updated.email.as_deref(), Some("alice@example.com"));

    let clear = ProfileUpdate { email: Some(String::new()), ..Default::default() };
    assert_eq!(auth.update_profile(alice.id, clear).await.unwrap().email, None);
}

#[tokio::test]
async fn repeated_failures_make_the_account_wait_but_never_lock_it() {
    let (_db, auth) = auth().await;
    auth.create_user(account("alice"), false).await.unwrap();

    for _ in 0..3 {
        let err = auth.login("password", "alice", "wrong-password", None).await.unwrap_err();
        assert_eq!(err.code, "INVALID_CREDENTIALS", "the first three failures cost nothing");
    }
    let err = auth.login("password", "alice", "password123", None).await.unwrap_err();
    assert_eq!(err.code, "TOO_MANY_ATTEMPTS", "even the right password waits");
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    assert!(auth.login("password", "alice", "password123", None).await.is_ok(), "after the wait the owner gets in");
}

#[tokio::test]
async fn a_burst_of_attempts_at_once_is_held_to_the_free_ones() {
    let (_db, auth) = auth().await;
    auth.create_user(account("alice"), false).await.unwrap();

    let attempts = (0..10).map(|_| auth.login("password", "alice", "wrong-password", None));
    let results = futures_util::future::join_all(attempts).await;
    let checked = results.iter().filter(|r| r.as_ref().is_err_and(|e| e.code == "INVALID_CREDENTIALS")).count();
    assert_eq!(checked, 3, "only the free attempts are checked, the rest wait");
}

#[tokio::test]
async fn one_address_failing_on_many_accounts_is_blocked() {
    let (_db, auth) = auth_with(AccountSettings { max_login_failures: 1, ..Default::default() }).await;
    auth.create_user(account("alice"), false).await.unwrap();
    let attacker = Some(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7)));

    // Five times the per-account limit, each on a different account.
    for i in 0..5 {
        assert!(auth.login("password", &format!("user{i}"), "guess", attacker).await.is_err());
    }
    let err = auth.login("password", "alice", "password123", attacker).await.unwrap_err();
    assert_eq!(err.code, "TOO_MANY_ATTEMPTS");
    assert!(auth.login("password", "alice", "password123", None).await.is_ok(), "other clients are not blocked");
}

#[tokio::test]
async fn settings_changes_apply_to_the_running_service() {
    let settings = Arc::new(Live::new(AccountSettings::default()));
    let db = test_db().await;
    let auth = Auth::new(db.conn.clone(), settings.clone());
    auth.register(account("admin")).await.unwrap();
    assert!(!auth.status().await.unwrap().registration_open);

    settings.set(AccountSettings { open_registration: true, default_quota_mib: Some(5), ..Default::default() });
    assert!(auth.status().await.unwrap().registration_open);
    let bob = auth.register(account("bob")).await.unwrap();
    assert_eq!(bob.quota_bytes, Some(5 * 1024 * 1024));
}

fn calendar_password(access: Access) -> NewAppPassword {
    NewAppPassword { name: "Phone".into(), scopes: vec![Scope { app: "calendar".into(), access }] }
}

#[tokio::test]
async fn app_passwords_open_only_their_owner_and_scopes() {
    let (_db, auth) = auth().await;
    let alice = auth.create_user(account("alice"), false).await.unwrap();
    auth.create_user(account("bob"), false).await.unwrap();
    let created = auth.create_app_password(&alice, calendar_password(Access::Read)).await.unwrap();

    let found = auth.app_password_user("Alice", &created.password, None).await.unwrap().unwrap();
    assert_eq!(found.user.id, alice.id);
    assert_eq!(found.access("calendar"), Some(Access::Read));
    assert_eq!(found.access("notes"), None);

    // The password belongs to alice, not to whoever names it.
    assert!(auth.app_password_user("bob", &created.password, None).await.unwrap().is_none());
    // The account password is not an app password.
    assert!(auth.app_password_user("alice", "password123", None).await.unwrap().is_none());
    // An app password does not log in to the web UI.
    assert!(auth.login("password", "alice", &created.password, None).await.is_err());

    let listed = auth.app_passwords(&alice).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert!(listed[0].last_used_at.is_some(), "use is recorded");
}

#[tokio::test]
async fn revoked_app_passwords_and_disabled_users_stop_working() {
    let (_db, auth) = auth().await;
    let admin = auth.create_user(account("admin"), true).await.unwrap();
    let alice = auth.create_user(account("alice"), false).await.unwrap();
    let first = auth.create_app_password(&alice, calendar_password(Access::Write)).await.unwrap();
    let second = auth.create_app_password(&alice, calendar_password(Access::Write)).await.unwrap();

    let err = auth.revoke_app_password(&admin, first.info.id).await.unwrap_err();
    assert_eq!(err.code, "APP_PASSWORD_NOT_FOUND", "only the owner revokes");
    auth.revoke_app_password(&alice, first.info.id).await.unwrap();
    assert!(auth.app_password_user("alice", &first.password, None).await.unwrap().is_none());
    assert!(auth.app_password_user("alice", &second.password, None).await.unwrap().is_some());

    let disable = AccountUpdate { disabled: Some(true), ..Default::default() };
    auth.update_account(&admin, alice.id, disable).await.unwrap();
    assert!(auth.app_password_user("alice", &second.password, None).await.unwrap().is_none());
}

#[tokio::test]
async fn app_passwords_are_validated() {
    let (_db, auth) = auth().await;
    let alice = auth.create_user(account("alice"), false).await.unwrap();
    let no_name = NewAppPassword { name: " ".into(), ..calendar_password(Access::Read) };
    assert_eq!(auth.create_app_password(&alice, no_name).await.unwrap_err().code, "INVALID_NAME");
    let no_scopes = NewAppPassword { scopes: vec![], ..calendar_password(Access::Read) };
    assert_eq!(auth.create_app_password(&alice, no_scopes).await.unwrap_err().code, "INVALID_SCOPES");
    let mut twice = calendar_password(Access::Read);
    twice.scopes.push(Scope { app: "calendar".into(), access: Access::Write });
    assert_eq!(auth.create_app_password(&alice, twice).await.unwrap_err().code, "INVALID_SCOPES");
}

#[tokio::test]
async fn guessing_app_passwords_blocks_the_guesser_not_the_account() {
    let (_db, auth) = auth_with(AccountSettings { max_login_failures: 1, ..Default::default() }).await;
    let alice = auth.create_user(account("alice"), false).await.unwrap();
    let created = auth.create_app_password(&alice, calendar_password(Access::Read)).await.unwrap();
    let attacker = Some(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7)));
    let phone = Some(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 1)));
    let mut blocked = false;
    for _ in 0..20 {
        match auth.app_password_user("alice", "guess", attacker).await {
            Ok(found) => assert!(found.is_none()),
            Err(err) => {
                assert_eq!(err.code, "TOO_MANY_ATTEMPTS");
                blocked = true;
                break;
            }
        }
    }
    assert!(blocked, "the guessing address is blocked");
    assert!(auth.app_password_user("alice", &created.password, phone).await.unwrap().is_some(), "alice's phone isn't");
}

fn external(subject: &str, admin: Option<bool>) -> rustcloud_auth::ExternalLogin {
    rustcloud_auth::ExternalLogin {
        identity: rustcloud_auth::Identity { provider: "oidc:corp".into(), subject: subject.into() },
        username: Some("Admin".into()),
        auto_create: true,
        admin,
        ..Default::default()
    }
}

#[tokio::test]
async fn external_logins_create_accounts_with_free_names_and_never_demote_the_last_admin() {
    let (_db, auth) = auth().await;
    auth.register(account("admin")).await.unwrap();

    // "admin" is taken, so the new account gets the next free name.
    let first = auth.sign_in_external(&external("sub-1", Some(true))).await.unwrap();
    assert_eq!(first.user.username, "admin-2");
    assert!(first.user.is_admin);

    // Two administrators: the service may demote one…
    let again = auth.sign_in_external(&external("sub-1", Some(false))).await.unwrap();
    assert_eq!(again.user.id, first.user.id, "the same account");
    assert!(!again.user.is_admin);

    // …but not the last one.
    let admin = auth.list_users().await.unwrap().into_iter().find(|u| u.username == "admin").unwrap();
    auth.update_account(
        &admin,
        first.user.id,
        rustcloud_auth::AccountUpdate { is_admin: Some(true), ..Default::default() },
    )
    .await
    .unwrap();
    auth.update_account(
        &first.user,
        admin.id,
        rustcloud_auth::AccountUpdate { is_admin: Some(false), ..Default::default() },
    )
    .await
    .unwrap();
    let last = auth.sign_in_external(&external("sub-1", Some(false))).await.unwrap();
    assert!(last.user.is_admin, "the last administrator stays one");

    // Without automatic accounts, unknown people are turned away.
    let closed = rustcloud_auth::ExternalLogin { auto_create: false, ..external("sub-2", None) };
    assert_eq!(auth.sign_in_external(&closed).await.unwrap_err().code, "NO_ACCOUNT");
}

/// The PKCE pair of a test client: the verifier and its S256 challenge.
fn pkce() -> (String, String) {
    use base64::Engine;
    use sha2::Digest;
    let verifier = "a-verifier-of-a-client-app-that-is-long-enough-0123456789".to_owned();
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(&verifier));
    (verifier, challenge)
}

const REDIRECT: &str = "http://127.0.0.1:53682/callback";

fn request(challenge: &str) -> AuthorizeRequest {
    AuthorizeRequest {
        client_id: "RustCloud Desktop".into(),
        device_name: Some("laptop".into()),
        redirect_uri: REDIRECT.into(),
        code_challenge: challenge.into(),
        code_challenge_method: Some("S256".into()),
    }
}

#[tokio::test]
async fn devices_log_in_through_the_browser_and_renew_their_tokens() {
    let (db, auth) = auth().await;
    let alice = auth.create_user(account("alice"), false).await.unwrap();
    let browser = auth.login("password", "alice", "password123", None).await.unwrap();
    let browser = auth.session_user(&browser.token).await.unwrap().unwrap();
    let (verifier, challenge) = pkce();

    // The code works once, with the verifier and the same redirect URI.
    let code = auth.authorize(&browser, &request(&challenge)).unwrap();
    let wrong = auth.exchange_code(&code, "x".repeat(43).as_str(), REDIRECT, None).await;
    assert_eq!(wrong.unwrap_err().code, "invalid_grant", "and a failed try uses it up");
    let code = auth.authorize(&browser, &request(&challenge)).unwrap();
    let other_uri = auth.exchange_code(&code, &verifier, "http://127.0.0.1:1/", None).await;
    assert_eq!(other_uri.unwrap_err().code, "invalid_grant");
    let code = auth.authorize(&browser, &request(&challenge)).unwrap();
    let tokens = auth.exchange_code(&code, &verifier, REDIRECT, None).await.unwrap();
    assert_eq!(auth.exchange_code(&code, &verifier, REDIRECT, None).await.unwrap_err().code, "invalid_grant");

    let device = auth.authenticate(&tokens.access_token, None).await.unwrap().unwrap();
    assert_eq!(device.user.id, alice.id);
    assert!(device.device);
    assert_eq!(device.browser_only().unwrap_err().code, "BROWSER_ONLY");
    assert_eq!(auth.authorize(&device, &request(&challenge)).unwrap_err().code, "BROWSER_ONLY");
    assert!(auth.session_user(&tokens.refresh_token).await.unwrap().is_none(), "a refresh token is no session");

    let sessions = auth.sessions(&browser).await.unwrap();
    assert_eq!(sessions.len(), 2);
    let listed = sessions.iter().find(|s| s.device).unwrap();
    assert_eq!(listed.name.as_deref(), Some("laptop (RustCloud Desktop)"));
    assert_eq!(listed.method.as_deref(), Some("password"));

    // Renewing replaces the refresh token; the old one works once more right
    // after (a retry), later it ends the session.
    let renewed = auth.refresh(&tokens.refresh_token, None).await.unwrap();
    assert_ne!(renewed.refresh_token, tokens.refresh_token);
    let retried = auth.refresh(&tokens.refresh_token, None).await.unwrap();
    assert!(auth.refresh(&renewed.refresh_token, None).await.is_err(), "the retry replaced it");
    assert!(auth.authenticate(&retried.access_token, None).await.unwrap().is_some());
    let newest = auth.refresh(&retried.refresh_token, None).await.unwrap();

    use sea_orm::EntityTrait;
    rustcloud_db::session::Entity::update_many()
        .col_expr(
            rustcloud_db::session::Column::RotatedAt,
            sea_orm::sea_query::Expr::value(chrono::Utc::now() - chrono::Duration::minutes(5)),
        )
        .exec(&db.conn)
        .await
        .unwrap();
    assert_eq!(auth.refresh(&retried.refresh_token, None).await.unwrap_err().code, "invalid_grant");
    assert!(auth.refresh(&newest.refresh_token, None).await.is_err(), "copied: the session is over");
    assert!(auth.authenticate(&newest.access_token, None).await.unwrap().is_none(), "at once");
}

#[tokio::test]
async fn ending_a_session_or_the_browser_one_stops_the_device() {
    let (_db, auth) = auth().await;
    auth.create_user(account("alice"), false).await.unwrap();
    let login = auth.login("password", "alice", "password123", None).await.unwrap();
    let browser = auth.session_user(&login.token).await.unwrap().unwrap();
    let (verifier, challenge) = pkce();

    let code = auth.authorize(&browser, &request(&challenge)).unwrap();
    let tokens = auth.exchange_code(&code, &verifier, REDIRECT, None).await.unwrap();
    let device = auth.authenticate(&tokens.access_token, None).await.unwrap().unwrap();
    assert_eq!(auth.end_session(&device, browser.session_id).await.unwrap_err().code, "BROWSER_ONLY");
    auth.end_session(&browser, device.session_id).await.unwrap();
    assert!(auth.authenticate(&tokens.access_token, None).await.unwrap().is_none());
    assert!(auth.refresh(&tokens.refresh_token, None).await.is_err());

    // Logging out in the browser before the code is exchanged takes it back.
    let code = auth.authorize(&browser, &request(&challenge)).unwrap();
    auth.logout(browser.session_id).await.unwrap();
    assert!(auth.exchange_code(&code, &verifier, REDIRECT, None).await.is_err());
}
