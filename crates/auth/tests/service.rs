//! Behaviour of the auth service against a real database.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

use rustcloud_auth::{Access, AccountSettings, AccountUpdate, Auth, NewAccount, NewAppPassword, ProfileUpdate, Scope};
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

    let err = auth.change_password(&laptop_session, "wrong-current", "new-password-1").await.unwrap_err();
    assert_eq!(err.code, "WRONG_PASSWORD");

    auth.change_password(&laptop_session, "password123", "new-password-1").await.unwrap();
    assert!(auth.session_user(&laptop.token).await.unwrap().is_some());
    assert!(auth.session_user(&phone.token).await.unwrap().is_none());
    assert!(auth.login("password", "alice", "password123", None).await.is_err());
    assert!(auth.login("password", "alice", "new-password-1", None).await.is_ok());
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
async fn repeated_failures_block_logins_for_that_account() {
    let (_db, auth) = auth_with(AccountSettings { max_login_failures: 3, ..Default::default() }).await;
    auth.create_user(account("alice"), false).await.unwrap();

    for _ in 0..3 {
        assert!(auth.login("password", "alice", "wrong-password", None).await.is_err());
    }
    let err = auth.login("password", "alice", "password123", None).await.unwrap_err();
    assert_eq!(err.code, "TOO_MANY_ATTEMPTS", "even the right password is refused while blocked");
}

#[tokio::test]
async fn a_burst_of_attempts_at_once_is_held_to_the_limit() {
    let (_db, auth) = auth_with(AccountSettings { max_login_failures: 3, ..Default::default() }).await;
    auth.create_user(account("alice"), false).await.unwrap();

    let attempts = (0..10).map(|_| auth.login("password", "alice", "wrong-password", None));
    let results = futures_util::future::join_all(attempts).await;
    let checked = results.iter().filter(|r| r.as_ref().is_err_and(|e| e.code == "INVALID_CREDENTIALS")).count();
    assert_eq!(checked, 3, "only as many passwords are checked as the limit allows");
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
