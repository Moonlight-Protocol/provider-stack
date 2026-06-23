//! UC5: the standin honours its own removal from a council via the pull path.
//!
//! Covers boot convergence-by-query: when the council's authoritative
//! membership-status endpoint reports the PP is no longer a member (404), the
//! local membership row is demoted to REJECTED; a still-active PP is untouched.
//!
//! Wiremock fakes the council; a real per-test Postgres exercises the DB writes.

mod common;

use common::{build_test_app_state, pp_strkey, TestDb};
use provider_stack_core::pipelines::membership_convergence::converge_membership_statuses;
use provider_stack_persistence::{CouncilMembershipRepo, CouncilMembershipStatus};
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SERVICE_DOMAIN: &str = "smoke.local";
const CHANNEL_AUTH_ID: &str = "CABCXYZ123";

async fn skip_if_no_db() -> Option<TestDb> {
    if std::env::var("DATABASE_URL").is_err() {
        eprintln!("DATABASE_URL not set — skipping council-removed integration test");
        return None;
    }
    Some(TestDb::create().await)
}

/// Seed one ACTIVE membership whose council_url points at the wiremock council.
async fn seed_active_membership(db: &TestDb, council_url: &str) {
    let repo = CouncilMembershipRepo::new(db.pool.clone());
    repo.create(
        &Uuid::new_v4().to_string(),
        council_url,
        "GCOUNCIL",
        CHANNEL_AUTH_ID,
        Some("[\"US\"]"),
    )
    .await
    .expect("seed membership");
    repo.set_status(CHANNEL_AUTH_ID, CouncilMembershipStatus::Active)
        .await
        .expect("promote to active");
}

async fn membership_status(db: &TestDb) -> String {
    sqlx::query_scalar("SELECT status::text FROM council_memberships WHERE channel_auth_id = $1")
        .bind(CHANNEL_AUTH_ID)
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

/// Council says the PP is gone (404). Mirrors a `provider_removed` that landed
/// while the standin was down: boot convergence must demote it to REJECTED.
async fn mount_not_a_member(council: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/api/v1/public/provider/membership-status"))
        .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
            "status": "NOT_FOUND"
        })))
        .mount(council)
        .await;
}

/// Council still reports the PP active (200). A spurious/stale notice must not
/// knock a valid membership offline.
async fn mount_still_active(council: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/api/v1/public/provider/membership-status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "ACTIVE"
        })))
        .mount(council)
        .await;
}

#[actix_web::test]
async fn boot_convergence_demotes_membership_when_council_reports_removed() {
    let Some(db) = skip_if_no_db().await else {
        return;
    };
    let council = MockServer::start().await;
    mount_not_a_member(&council).await;
    seed_active_membership(&db, &council.uri()).await;

    let state = build_test_app_state(
        [0xABu8; 32],
        pp_strkey([0xCCu8; 32]),
        db.pool.clone(),
        SERVICE_DOMAIN,
    );

    let conv = converge_membership_statuses(&state.config, &db.pool)
        .await
        .expect("convergence");
    assert_eq!(conv.updated, 1);
    assert_eq!(membership_status(&db).await, "REJECTED");

    db.cleanup().await;
}

#[actix_web::test]
async fn convergence_leaves_active_membership_untouched() {
    let Some(db) = skip_if_no_db().await else {
        return;
    };
    let council = MockServer::start().await;
    mount_still_active(&council).await;
    seed_active_membership(&db, &council.uri()).await;

    let state = build_test_app_state(
        [0xABu8; 32],
        pp_strkey([0xCCu8; 32]),
        db.pool.clone(),
        SERVICE_DOMAIN,
    );

    let conv = converge_membership_statuses(&state.config, &db.pool)
        .await
        .expect("convergence");
    assert_eq!(conv.updated, 0);
    assert_eq!(membership_status(&db).await, "ACTIVE");

    db.cleanup().await;
}
