use arenax_backend::api_error::ApiError;
use arenax_backend::models::ReportScoreRequest;
use arenax_backend::service::MatchService;
use sqlx::{PgPool, Row};
use uuid::Uuid;

async fn test_pool() -> Option<PgPool> {
    let database_url = std::env::var("TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgresql://test:test@localhost/arenax_test".to_string());
    let pool = PgPool::connect(&database_url).await.ok()?;
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("apply backend migrations to score submission test database");
    Some(pool)
}

async fn create_match(pool: &PgPool) -> (Uuid, Uuid, Uuid) {
    let match_id = Uuid::new_v4();
    let player1_id = create_player(pool).await;
    let player2_id = create_player(pool).await;
    sqlx::query(
        "INSERT INTO matches (id, match_type, status, player1_id, player2_id, game_mode) VALUES ($1, 2, 2, $2, $3, 'score-idempotency-test')",
    )
    .bind(match_id)
    .bind(player1_id)
    .bind(player2_id)
    .execute(pool)
    .await
    .expect("insert test match");
    (match_id, player1_id, player2_id)
}

async fn create_player(pool: &PgPool) -> Uuid {
    let id = Uuid::new_v4();
    let compact_id = id.simple().to_string();
    sqlx::query(
        "INSERT INTO users (id, phone_number, username) VALUES ($1, $2, $3)",
    )
    .bind(id)
    .bind(format!("+{}", &compact_id[..19]))
    .bind(format!("score_test_{id}"))
    .execute(pool)
    .await
    .expect("insert test player");
    id
}

fn report(score: i32, opponent_score: i32, key: &str) -> ReportScoreRequest {
    ReportScoreRequest {
        score,
        opponent_score,
        proof_url: None,
        telemetry_data: None,
        idempotency_key: Some(key.to_string()),
    }
}

async fn cleanup(pool: &PgPool, match_id: Uuid, player1_id: Uuid, player2_id: Uuid) {
    sqlx::query("DELETE FROM matches WHERE id = $1")
        .bind(match_id)
        .execute(pool)
        .await
        .expect("delete test match");
    sqlx::query("DELETE FROM users WHERE id IN ($1, $2)")
        .bind(player1_id)
        .bind(player2_id)
        .execute(pool)
        .await
        .expect("delete test players");
}

#[tokio::test]
async fn concurrent_duplicate_and_conflicting_replays_are_stable() {
    let Some(pool) = test_pool().await else {
        eprintln!("skipping: no TEST_DATABASE_URL database is reachable");
        return;
    };
    let (match_id, player1_id, player2_id) = create_match(&pool).await;
    let service = MatchService::new(pool.clone());

    let (first, retry) = tokio::join!(
        service.report_score(match_id, player1_id, report(3, 1, "duplicate-key")),
        service.report_score(match_id, player1_id, report(3, 1, "duplicate-key")),
    );
    let first = first.expect("first concurrent submission succeeds");
    let retry = retry.expect("same concurrent submission returns original");
    assert_eq!(first.id, retry.id);
    assert_eq!(first.request_hash, retry.request_hash);

    let conflicting = service
        .report_score(match_id, player1_id, report(2, 1, "duplicate-key"))
        .await;
    let conflict = conflicting.expect_err("changed replay must conflict");
    assert!(matches!(&conflict, ApiError::Conflict(_)));
    assert_eq!(
        conflict.to_string(),
        "Conflict: Idempotency key was already used for a different score submission"
    );

    let reports: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM match_scores WHERE match_id = $1")
    .bind(match_id)
    .fetch_one(&pool)
    .await
    .expect("count submitted reports");
    assert_eq!(reports, 1);
    let row = sqlx::query("SELECT player_id, request_hash FROM match_scores WHERE match_id = $1")
        .bind(match_id)
        .fetch_one(&pool)
        .await
        .expect("read submitted report audit");
    assert_eq!(row.get::<Uuid, _>("player_id"), player1_id);
    assert_eq!(row.get::<Option<String>, _>("request_hash"), first.request_hash);

    cleanup(&pool, match_id, player1_id, player2_id).await;
}

#[tokio::test]
async fn competing_participants_preserve_both_reports_and_conflict_status() {
    let Some(pool) = test_pool().await else {
        eprintln!("skipping: no TEST_DATABASE_URL database is reachable");
        return;
    };
    let (match_id, player1_id, player2_id) = create_match(&pool).await;
    let service = MatchService::new(pool.clone());

    let (player1, player2) = tokio::join!(
        service.report_score(match_id, player1_id, report(5, 1, "player-one-key")),
        service.report_score(match_id, player2_id, report(4, 0, "player-two-key")),
    );
    player1.expect("player one's report is accepted");
    player2.expect("player two's report is accepted");

    let row = sqlx::query(
        "SELECT status, COUNT(s.id) AS reports, COUNT(s.request_hash) AS hashed_reports FROM matches m JOIN match_scores s ON s.match_id = m.id WHERE m.id = $1 GROUP BY m.status",
    )
    .bind(match_id)
    .fetch_one(&pool)
    .await
    .expect("read final match and reports");
    assert_eq!(row.get::<i32, _>("status"), 4);
    assert_eq!(row.get::<i64, _>("reports"), 2);
    assert_eq!(row.get::<i64, _>("hashed_reports"), 2);

    let audited_actors: i64 = sqlx::query_scalar(
        "SELECT COUNT(DISTINCT player_id) FROM score_report_attempts WHERE match_id = $1 AND accepted = TRUE AND request_hash IS NOT NULL",
    )
    .bind(match_id)
    .fetch_one(&pool)
    .await
    .expect("read accepted report audit");
    assert_eq!(audited_actors, 2);

    cleanup(&pool, match_id, player1_id, player2_id).await;
}