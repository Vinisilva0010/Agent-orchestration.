//! Accounts are unique by email, audited without the password hash, and only
//! the first administrator can be created by the bootstrap command.

use secrecy::SecretString;
use server::accounts::{self, AccountError, Role};
use sqlx::PgPool;

const PASSPHRASE: &str = "correct horse battery staple";

fn secret(value: &str) -> SecretString {
    SecretString::from(value.to_string())
}

#[sqlx::test(migrations = false)]
async fn accounts_are_unique_by_email_and_audited_without_the_hash(pool: PgPool) {
    server::migrate_all(&pool).await.unwrap();
    let id = accounts::create_user(
        &pool,
        "user:1",
        " Alice@Example.COM ",
        "Alice",
        Role::Member,
        &secret(PASSPHRASE),
    )
    .await
    .unwrap();
    let email: String = sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(email, "alice@example.com");
    assert_eq!(
        accounts::create_user(
            &pool,
            "user:1",
            "ALICE@example.com",
            "Other",
            Role::Viewer,
            &secret(PASSPHRASE)
        )
        .await,
        Err(AccountError::EmailTaken)
    );

    let (actor, details): (String, String) = sqlx::query_as(
        "SELECT actor, details::text FROM audit_events WHERE action = 'user.created'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(actor, "user:1");
    assert!(details.contains("alice@example.com"), "{details}");
    assert!(
        !details.contains("argon2"),
        "password hash in the audit log: {details}"
    );
}

#[sqlx::test(migrations = false)]
async fn only_the_first_administrator_can_be_bootstrapped(pool: PgPool) {
    server::migrate_all(&pool).await.unwrap();
    accounts::bootstrap_admin(&pool, "admin@example.com", "Admin", &secret(PASSPHRASE))
        .await
        .unwrap();
    assert_eq!(
        accounts::bootstrap_admin(&pool, "second@example.com", "Second", &secret(PASSPHRASE)).await,
        Err(AccountError::AdminExists)
    );
    let actor: String =
        sqlx::query_scalar("SELECT actor FROM audit_events WHERE action = 'user.created'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(actor, "installer");
}
