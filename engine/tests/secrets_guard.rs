//! The engine refuses to start with the gateway admin secret.

use engine::secrets_guard::find_problems;

#[test]
fn the_engine_refuses_the_gateway_admin_secret() {
    assert_eq!(
        find_problems([("GATEWAY_ADMIN_SECRET", "x".repeat(40))]).len(),
        1
    );
    assert!(find_problems([("GATEWAY_ISSUER_SECRET", "x".repeat(40))]).is_empty());
}
