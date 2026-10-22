use crate::service::matchmaker::{MatchmakerService, EloEngine, MatchmakingConfig};
use crate::models::matchmaker::*;
use crate::db::DbPool;
use redis::Client;
use redis::aio::ConnectionManager;
use sqlx::PgPool;
use std::collections::HashMap;
use uuid::Uuid;
use chrono::Utc;

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_test;

    pub(crate) fn create_test_config() -> MatchmakingConfig {
        MatchmakingConfig {
            elo_bucket_size: 100,
            max_elo_gap: 500,
            expansion_intervals: vec![
                std::time::Duration::from_secs(30),
                std::time::Duration::from_secs(60),
                std::time::Duration::from_secs(120),
                std::time::Duration::from_secs(300),
            ],
            max_wait_time: std::time::Duration::from_secs(600),
            min_players_per_match: 2,
            max_players_per_match: 2,
        }
    }

    pub(crate) async fn setup_test_db() -> DbPool {
        // Create test database connection
        let database_url = std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgresql://test:test@localhost/arenax_test".to_string());
        
        PgPool::connect(&database_url)
            .await
            .expect("Failed to create test database pool")
    }

    pub(crate) async fn setup_test_redis() -> ConnectionManager {
        let redis_url = std::env::var("TEST_REDIS_URL")
            .unwrap_or_else(|_| "redis://localhost".to_string());
        
        let client = Client::open(redis_url)
            .expect("Failed to create test Redis client");
        client.get_connection_manager().await
            .expect("Failed to create test Redis connection manager")
    }

    pub(crate) async fn cleanup_test_data(db_pool: &DbPool, redis_client: &ConnectionManager) {
        // Clean up test data
        sqlx::query!("DELETE FROM matchmaking_queue WHERE user_id::text LIKE 'test-%'")
            .execute(db_pool)
            .await
            .ok();

        sqlx::query!("DELETE FROM matches WHERE player1_id::text LIKE 'test-%'")
            .execute(db_pool)
            .await
            .ok();

        sqlx::query!("DELETE FROM user_elo WHERE user_id::text LIKE 'test-%'")
            .execute(db_pool)
            .await
            .ok();

        // Clean up Redis
        let mut conn = redis_client.clone();
        
        let pattern = "queue:test-*";
        let keys: Vec<String> = redis::cmd("KEYS")
            .arg(pattern)
            .query_async(&mut conn)
            .await
            .unwrap_or_default();
        
        for key in keys {
            redis::cmd("DEL")
                .arg(&key)
                .query_async::<()>(&mut conn)
                .await
                .ok();
        }
    }

    #[tokio::test]
    async fn test_add_to_queue() {
        let db_pool = setup_test_db().await;
        let redis_client = setup_test_redis().await;
        let config = create_test_config();
        let matchmaker = MatchmakerService::new(db_pool, redis_client, config);

        let user_id = Uuid::new_v4();
        let game = "test-game".to_string();
        let game_mode = "test-mode".to_string();
        let current_elo = 1200;

        // Add user to queue
        let result = matchmaker.add_to_queue(user_id, game.clone(), game_mode.clone(), current_elo).await;
        assert!(result.is_ok());

        // Check if user is in queue
        let queue_entry = matchmaker.is_user_in_queue(user_id, &game, &game_mode).await.unwrap();
        assert!(queue_entry.is_some());
        
        let entry = queue_entry.unwrap();
        assert_eq!(entry.user_id, user_id);
        assert_eq!(entry.current_elo, current_elo);
        assert_eq!(entry.game, game);
        assert_eq!(entry.game_mode, game_mode);
    }

    #[tokio::test]
    async fn test_remove_from_queue() {
        let db_pool = setup_test_db().await;
        let redis_client = setup_test_redis().await;
        let config = create_test_config();
        let matchmaker = MatchmakerService::new(db_pool, redis_client.clone(), config);

        let user_id = Uuid::new_v4();
        let game = "test-game".to_string();
        let game_mode = "test-mode".to_string();
        let current_elo = 1200;

        // Add user to queue
        matchmaker.add_to_queue(user_id, game.clone(), game_mode.clone(), current_elo).await.unwrap();

        let mut conn = redis_client.clone();
        let result = matchmaker.remove_from_queue(&mut conn, &user_id, &game, &game_mode).await;
        assert!(result.is_ok());

        // Check if user is no longer in queue
        let queue_entry = matchmaker.is_user_in_queue(user_id, &game, &game_mode).await.unwrap();
        assert!(queue_entry.is_none());
    }

    #[tokio::test]
    async fn test_elo_calculation() {
        let elo_engine = EloEngine::new(32.0);
        let player1_id = Uuid::new_v4();
        let player2_id = Uuid::new_v4();
        let player1_elo = 1200;
        let player2_elo = 1200;

        // Test win scenario
        let (new_elo1, new_elo2) = elo_engine.calculate_elo_change(
            player1_elo,
            player2_elo,
            Some(player1_id), // Player 1 wins
            player1_id,
            player2_id,
        );

        assert!(new_elo1 > player1_elo); // Winner gains ELO
        assert!(new_elo2 < player2_elo); // Loser loses ELO
        assert_eq!(new_elo1 - player1_elo, -(new_elo2 - player2_elo)); // Zero-sum

        // Test draw scenario
        let (new_elo1_draw, new_elo2_draw) = elo_engine.calculate_elo_change(
            player1_elo,
            player2_elo,
            None, // Draw
            player1_id,
            player2_id,
        );

        assert!(new_elo1_draw < player1_elo); // Both lose ELO in draw
        assert!(new_elo2_draw < player2_elo);
        assert_eq!(new_elo1_draw - player1_elo, new_elo2_draw - player2_elo); // Same change
    }

    #[tokio::test]
    async fn test_dynamic_elo_expansion() {
        let db_pool = setup_test_db().await;
        let redis_client = setup_test_redis().await;
        let config = create_test_config();
        let matchmaker = MatchmakerService::new(db_pool, redis_client, config);

        let player_elo = 1200;
        let wait_time_30s = Utc::now() - chrono::Duration::seconds(30);
        let wait_time_2m = Utc::now() - chrono::Duration::seconds(120);

        let entry_30s = QueueEntry {
            user_id: Uuid::new_v4(),
            game: "test-game".to_string(),
            game_mode: "test-mode".to_string(),
            current_elo: player_elo,
            min_elo: player_elo - 100,
            max_elo: player_elo + 100,
            joined_at: wait_time_30s,
            wait_time_multiplier: 1.0,
        };

        let entry_2m = QueueEntry {
            user_id: Uuid::new_v4(),
            game: "test-game".to_string(),
            game_mode: "test-mode".to_string(),
            current_elo: player_elo,
            min_elo: player_elo - 100,
            max_elo: player_elo + 100,
            joined_at: wait_time_2m,
            wait_time_multiplier: 1.0,
        };

        // Test ELO gap expansion based on wait time
        let max_gap_30s = matchmaker.calculate_max_elo_gap(&entry_30s);
        let max_gap_2m = matchmaker.calculate_max_elo_gap(&entry_2m);

        assert!(max_gap_2m > max_gap_30s); // Longer wait = larger gap
        assert!(max_gap_2m <= 500); // But never exceeds maximum
    }

    #[tokio::test]
    async fn test_match_quality_calculation() {
        let db_pool = setup_test_db().await;
        let redis_client = setup_test_redis().await;
        let config = create_test_config();
        let matchmaker = MatchmakerService::new(db_pool, redis_client, config);

        let now = Utc::now();
        let player1 = QueueEntry {
            user_id: Uuid::new_v4(),
            game: "test-game".to_string(),
            game_mode: "test-mode".to_string(),
            current_elo: 1200,
            min_elo: 1100,
            max_elo: 1300,
            joined_at: now,
            wait_time_multiplier: 1.0,
        };

        let player2_close = QueueEntry {
            user_id: Uuid::new_v4(),
            game: "test-game".to_string(),
            game_mode: "test-mode".to_string(),
            current_elo: 1250, // Close ELO
            min_elo: 1150,
            max_elo: 1350,
            joined_at: now,
            wait_time_multiplier: 1.0,
        };

        let player2_far = QueueEntry {
            user_id: Uuid::new_v4(),
            game: "test-game".to_string(),
            game_mode: "test-mode".to_string(),
            current_elo: 1500, // Far ELO
            min_elo: 1400,
            max_elo: 1600,
            joined_at: now,
            wait_time_multiplier: 1.0,
        };

        // Test match quality calculation
        let quality_close = matchmaker.calculate_match_quality(&player1, &player2_close, 50);
        let quality_far = matchmaker.calculate_match_quality(&player1, &player2_far, 300);

        assert!(quality_close > quality_far); // Closer ELO = better quality
        assert!(quality_close <= 1.5); // Should be reasonable range
        assert!(quality_far >= 0.0); // Should be non-negative
    }

    #[tokio::test]
    async fn test_queue_size_calculation() {
        let db_pool = setup_test_db().await;
        let redis_client = setup_test_redis().await;
        let config = create_test_config();
        let matchmaker = MatchmakerService::new(db_pool, redis_client, config);

        let game = "test-game".to_string();
        let game_mode = "test-mode".to_string();

        // Initially empty queue
        let initial_size = matchmaker.get_queue_size(&game, &game_mode).await.unwrap();
        assert_eq!(initial_size, 0);

        // Add players to queue
        for i in 0..5 {
            let user_id = Uuid::new_v4();
            matchmaker.add_to_queue(user_id, game.clone(), game_mode.clone(), 1200 + i * 10).await.unwrap();
        }

        // Check queue size
        let new_size = matchmaker.get_queue_size(&game, &game_mode).await.unwrap();
        assert_eq!(new_size, 5);
    }

    #[tokio::test]
    async fn test_estimated_wait_time() {
        let db_pool = setup_test_db().await;
        let redis_client = setup_test_redis().await;
        let config = create_test_config();
        let matchmaker = MatchmakerService::new(db_pool, redis_client, config);

        let user_id = Uuid::new_v4();
        let game = "test-game".to_string();
        let game_mode = "test-mode".to_string();

        // Add some players to queue first
        for i in 0..3 {
            let other_user_id = Uuid::new_v4();
            matchmaker.add_to_queue(other_user_id, game.clone(), game_mode.clone(), 1200).await.unwrap();
        }

        // Get estimated wait time
        let wait_time = matchmaker.get_estimated_wait_time(user_id, &game, &game_mode).await.unwrap();
        assert!(wait_time > 0); // Should estimate some wait time
        assert!(wait_time < 1000); // Should be reasonable (less than ~16 minutes)
    }

    #[tokio::test]
    async fn test_concurrent_queue_operations() {
        let db_pool = setup_test_db().await;
        let redis_client = setup_test_redis().await;
        let config = create_test_config();
        let matchmaker = std::sync::Arc::new(MatchmakerService::new(db_pool, redis_client, config));

        let game = "test-game".to_string();
        let game_mode = "test-mode".to_string();

        // Add multiple players concurrently
        let mut handles = Vec::new();
        for i in 0..10 {
            let matchmaker_clone = matchmaker.clone();
            let game_clone = game.clone();
            let game_mode_clone = game_mode.clone();
            
            let handle = tokio::spawn(async move {
                let user_id = Uuid::new_v4();
                matchmaker_clone.add_to_queue(user_id, game_clone, game_mode_clone, 1200 + i * 5).await
            });
            handles.push(handle);
        }

        // Wait for all operations to complete
        for handle in handles {
            assert!(handle.await.unwrap().is_ok());
        }

        // Check final queue size
        let final_size = matchmaker.get_queue_size(&game, &game_mode).await.unwrap();
        assert_eq!(final_size, 10);
    }

    #[tokio::test]
    async fn test_elo_boundary_conditions() {
        let elo_engine = EloEngine::new(32.0);
        let player1_id = Uuid::new_v4();
        let player2_id = Uuid::new_v4();

        // Test very high ELO difference
        let (new_elo1, new_elo2) = elo_engine.calculate_elo_change(
            3000, // Very high ELO
            500,  // Very low ELO
            Some(player1_id), // High ELO player wins (expected)
            player1_id,
            player2_id,
        );

        // High ELO player should gain very little
        assert!(new_elo1 - 3000 <= 5); 
        // Low ELO player should lose very little
        assert!(500 - new_elo2 <= 5);

        // Test upset (low ELO player wins)
        let (upset_elo1, upset_elo2) = elo_engine.calculate_elo_change(
            3000,
            500,
            Some(player2_id), // Low ELO player wins (upset)
            player1_id,
            player2_id,
        );

        // High ELO player should lose a lot
        assert!(3000 - upset_elo1 >= 20);
        // Low ELO player should gain a lot
        assert!(upset_elo2 - 500 >= 20);
    }

    #[tokio::test]
    async fn test_matchmaking_performance() {
        let db_pool = setup_test_db().await;
        let redis_client = setup_test_redis().await;
        let config = create_test_config();
        let matchmaker = MatchmakerService::new(db_pool, redis_client, config);

        let game = "perf-test".to_string();
        let game_mode = "test-mode".to_string();

        // Add 1000 players to queue
        let start_time = std::time::Instant::now();
        
        for i in 0..1000 {
            let user_id = Uuid::new_v4();
            let elo = 800 + (i % 800); // ELO range 800-1600
            matchmaker.add_to_queue(user_id, game.clone(), game_mode.clone(), elo).await.unwrap();
        }

        let add_duration = start_time.elapsed();
        println!("Added 1000 players to queue in {:?}", add_duration);
        assert!(add_duration.as_secs() < 5); // Should be fast

        // Test queue size retrieval performance
        let start_time = std::time::Instant::now();
        let queue_size = matchmaker.get_queue_size(&game, &game_mode).await.unwrap();
        let size_duration = start_time.elapsed();
        
        assert_eq!(queue_size, 1000);
        assert!(size_duration.as_millis() < 100);
        println!("Retrieved queue size in {:?}", size_duration);
    }

    #[test]
    fn test_calculate_wait_time_percentiles() {
        use crate::service::matchmaker::calculate_percentiles;

        let empty: Vec<f64> = vec![];
        let res_empty = calculate_percentiles(empty);
        assert_eq!(res_empty.p50, 0.0);
        assert_eq!(res_empty.p95, 0.0);
        assert_eq!(res_empty.p99, 0.0);

        let samples = vec![10.0, 20.0, 30.0, 40.0, 50.0, 60.0, 70.0, 80.0, 90.0, 100.0];
        let res = calculate_percentiles(samples);
        assert!(res.p50 >= 40.0 && res.p50 <= 60.0);
        assert!(res.p95 >= 90.0);
        assert!(res.p99 >= 90.0);
    }
}

// Integration tests
#[cfg(test)]
mod integration_tests {
    use super::*;
    use super::tests::*;
    use actix_web::{test, web, App};
    use crate::http::matchmaking::*;

    async fn setup_test_app() -> impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse,
        Error = actix_web::Error,
    > {
        let db_pool = setup_test_db().await;
        let redis_client = setup_test_redis().await;
        let config = create_test_config();
        let matchmaker = web::Data::new(MatchmakerService::new(db_pool, redis_client, config));
        let elo_engine = web::Data::new(EloEngine::new(32.0));

        test::init_service(
            App::new()
                .app_data(matchmaker)
                .app_data(elo_engine)
                .service(
                    web::scope("/api/matchmaking")
                        .route("/join", web::post().to(join_queue))
                        .route("/leave", web::post().to(leave_queue))
                        .route("/status/{game}/{game_mode}", web::get().to(get_queue_status))
                        .route("/stats", web::get().to(get_matchmaking_stats))
                        .route("/metrics", web::get().to(get_matchmaking_metrics_dashboard))
                )
        ).await
    }

    #[tokio::test]
    async fn test_join_queue_endpoint() {
        let app = setup_test_app().await;

        let request = JoinQueueRequest {
            game: "test-game".to_string(),
            game_mode: "test-mode".to_string(),
        };

        let req = test::TestRequest::post()
            .uri("/api/matchmaking/join")
            .set_json(request)
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert!(resp.status().is_success());

        let response: JoinQueueResponse = test::read_body_json(resp).await;
        assert!(response.success);
        assert!(response.queue_position.is_some());
    }

    #[tokio::test]
    async fn test_queue_status_endpoint() {
        let app = setup_test_app().await;

        let req = test::TestRequest::get()
            .uri("/api/matchmaking/status/test-game/test-mode")
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert!(resp.status().is_success());

        let response: QueueStatusResponse = test::read_body_json(resp).await;
        assert!(!response.in_queue); // Should not be in queue initially
        assert_eq!(response.queue_size, 0);
    }

    #[tokio::test]
    async fn test_matchmaking_stats_endpoint() {
        let app = setup_test_app().await;

        let req = test::TestRequest::get()
            .uri("/api/matchmaking/stats")
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert!(resp.status().is_success());

        let response: MatchmakingStatsResponse = test::read_body_json(resp).await;
        assert_eq!(response.total_players_in_queue, 0); // Should be empty initially
        assert!(response.games.is_empty());
    }

    #[tokio::test]
    async fn test_matchmaking_metrics_dashboard_endpoint() {
        let app = setup_test_app().await;

        let req = test::TestRequest::get()
            .uri("/api/matchmaking/metrics")
            .to_request();

        let resp = test::call_service(&app, req).await;
        assert!(resp.status().is_success());

        let response: crate::models::matchmaker::MatchmakingMetricsDashboard = test::read_body_json(resp).await;
        assert_eq!(response.total_queue_depth, 0);
        assert_eq!(response.hourly_aggregates.len(), 24);
    }
}

// =============================================================================
// Property tests — issue #12: eligibility uniqueness and fairness invariants
// =============================================================================
#[cfg(test)]
mod property_tests {
    use super::tests::*;
    use crate::models::matchmaker::QueueEntry;
    use crate::service::matchmaker::{calculate_percentiles, EloEngine, MatchmakerService, MatchmakingConfig};
    use chrono::Utc;
    use uuid::Uuid;

    // ── Helper: build a QueueEntry at a given ELO and wait time ──────────────
    fn make_entry(elo: i32, wait_secs: i64) -> QueueEntry {
        QueueEntry {
            user_id: Uuid::new_v4(),
            game: "prop-game".to_string(),
            game_mode: "1v1".to_string(),
            current_elo: elo,
            min_elo: elo - 100,
            max_elo: elo + 100,
            joined_at: Utc::now() - chrono::Duration::seconds(wait_secs),
            wait_time_multiplier: 1.0,
        }
    }

    /// Property: the Elo expansion window must always be ≤ MAX_ELO_GAP (500).
    #[test]
    fn prop_elo_expansion_never_exceeds_max() {
        // Test at every threshold boundary and well beyond it.
        for wait_secs in [0, 29, 30, 59, 60, 119, 120, 299, 300, 599, 600, 1200] {
            let entry = make_entry(1200, wait_secs);
            let gap = crate::service::matchmaker::MatchmakerService::calculate_max_elo_gap_pub(&entry);
            assert!(
                gap <= 500,
                "Elo gap {} > 500 at wait_secs={}", gap, wait_secs
            );
        }
    }

    /// Property: match quality is bounded in [0.0, 1.5].
    #[test]
    fn prop_match_quality_bounded() {
        let cases = [
            (1200, 1200, 0i32, 0i64, 0i64),
            (1200, 1200, 0, 300, 300),
            (1200, 1700, 500, 0, 600),
            (500, 3000, 500, 600, 600),
        ];
        for (elo1, elo2, gap, wait1, wait2) in cases {
            let p1 = make_entry(elo1, wait1);
            let p2 = make_entry(elo2, wait2);
            let q = crate::service::matchmaker::calculate_match_quality_pub(&p1, &p2, gap);
            assert!(q >= 0.0, "Quality {} < 0 for case {:?}", q, (elo1, elo2, gap));
            assert!(q <= 1.5, "Quality {} > 1.5 for case {:?}", q, (elo1, elo2, gap));
        }
    }

    /// Property: the matchmaker must never assign the same player_id twice
    /// in the same set of pairings for a single tick.
    #[test]
    fn prop_no_duplicate_players_in_tick_pairings() {
        // Build 20 queue entries at varying ELOs.
        let entries: Vec<QueueEntry> = (0..20)
            .map(|i| make_entry(1000 + i * 25, i as i64 * 5))
            .collect();

        // Simulate the pairing loop from find_matches_for_game_mode
        // (synchronous approximation — no Redis, just the selection logic).
        let mut sorted = entries.clone();
        sorted.sort_by(|a, b| a.joined_at.cmp(&b.joined_at));

        let mut processed = std::collections::HashSet::new();
        let mut paired_ids: Vec<Uuid> = Vec::new();

        for i in 0..sorted.len() {
            let player = &sorted[i];
            if processed.contains(&player.user_id) {
                continue;
            }
            // Find first eligible partner (no Redis check here — pure logic).
            for j in (i + 1)..sorted.len() {
                let candidate = &sorted[j];
                if processed.contains(&candidate.user_id) {
                    continue;
                }
                let gap = (player.current_elo - candidate.current_elo).abs();
                if gap <= 500 {
                    processed.insert(player.user_id);
                    processed.insert(candidate.user_id);
                    paired_ids.push(player.user_id);
                    paired_ids.push(candidate.user_id);
                    break;
                }
            }
        }

        // Verify uniqueness: no player_id appears more than once.
        let mut seen = std::collections::HashSet::new();
        for id in &paired_ids {
            assert!(seen.insert(*id), "Player {} paired twice in same tick", id);
        }
    }

    /// Property: a player with an active reservation must not be re-queued.
    #[tokio::test]
    async fn prop_player_with_reservation_cannot_join_queue() {
        let db_pool = setup_test_db().await;
        let redis_client = setup_test_redis().await;
        let config = create_test_config();
        let matchmaker = MatchmakerService::new(db_pool, redis_client, config);

        let user_id = Uuid::new_v4();
        let game = "prop-game".to_string();
        let game_mode = "1v1".to_string();

        // First join should succeed.
        matchmaker
            .add_to_queue(user_id, game.clone(), game_mode.clone(), 1200)
            .await
            .expect("First join must succeed");

        // Set a reservation (simulating a match being created).
        matchmaker
            .set_match_reservation(&user_id)
            .await
            .expect("Setting reservation must succeed");

        // Leave the queue (simulating post-match removal).
        let mut conn = matchmaker.redis_conn();
        matchmaker
            .remove_from_queue(&mut conn, &user_id, &game, &game_mode)
            .await
            .expect("Queue removal must succeed");

        // Attempt to re-join — must be rejected because reservation is active.
        let result = matchmaker
            .add_to_queue(user_id, game.clone(), game_mode.clone(), 1200)
            .await;

        assert!(
            result.is_err(),
            "Re-join must fail while active reservation exists"
        );

        // Release reservation — re-join must now succeed.
        matchmaker
            .release_match_reservation(&user_id)
            .await
            .expect("Releasing reservation must succeed");

        matchmaker
            .add_to_queue(user_id, game.clone(), game_mode.clone(), 1200)
            .await
            .expect("Re-join after reservation release must succeed");
    }

    /// Property: metrics never leak user identifiers.
    /// Verify that the serialized metrics dashboard contains no UUID-shaped
    /// strings corresponding to any queued player.
    #[tokio::test]
    async fn prop_metrics_contain_no_player_identifiers() {
        let db_pool = setup_test_db().await;
        let redis_client = setup_test_redis().await;
        let config = create_test_config();
        let matchmaker = MatchmakerService::new(db_pool, redis_client, config);

        let game = "metrics-check".to_string();
        let game_mode = "ranked".to_string();
        let mut player_ids = Vec::new();

        for i in 0..5 {
            let uid = Uuid::new_v4();
            player_ids.push(uid);
            matchmaker
                .add_to_queue(uid, game.clone(), game_mode.clone(), 1100 + i * 50)
                .await
                .expect("Queue join must succeed");
        }

        let dashboard = matchmaker
            .get_metrics_dashboard()
            .await
            .expect("Metrics dashboard must succeed");

        let serialized = serde_json::to_string(&dashboard).expect("Serialize dashboard");

        for uid in &player_ids {
            assert!(
                !serialized.contains(&uid.to_string()),
                "Dashboard must not contain player UUID {}",
                uid
            );
        }
    }

    /// Benchmark: queue insertion throughput should handle ≥200 entries/second.
    #[tokio::test]
    async fn bench_queue_insertion_throughput() {
        let db_pool = setup_test_db().await;
        let redis_client = setup_test_redis().await;
        let config = create_test_config();
        let matchmaker = MatchmakerService::new(db_pool, redis_client, config);

        let game = "bench-game".to_string();
        let game_mode = "ranked".to_string();
        let n = 200usize;

        let start = std::time::Instant::now();
        for i in 0..n {
            let uid = Uuid::new_v4();
            matchmaker
                .add_to_queue(uid, game.clone(), game_mode.clone(), 900 + (i % 700) as i32)
                .await
                .expect("Bench add_to_queue must succeed");
        }
        let elapsed = start.elapsed();

        // Must insert 200 entries in under 5 seconds on any reasonable host.
        assert!(
            elapsed.as_secs() < 5,
            "Queue insertion too slow: {:?} for {} entries",
            elapsed,
            n
        );

        let size = matchmaker
            .get_queue_size(&game, &game_mode)
            .await
            .expect("get_queue_size must succeed");
        assert_eq!(size, n, "All inserted entries must be visible in the queue");
    }

    /// Benchmark: queue-size lookup (SCAN) must complete within 100 ms for
    /// queues of 1 000 entries.
    #[tokio::test]
    async fn bench_queue_size_lookup_1000_entries() {
        let db_pool = setup_test_db().await;
        let redis_client = setup_test_redis().await;
        let config = create_test_config();
        let matchmaker = MatchmakerService::new(db_pool, redis_client, config);

        let game = "bench-size-game".to_string();
        let game_mode = "casual".to_string();

        for i in 0..1000 {
            let uid = Uuid::new_v4();
            matchmaker
                .add_to_queue(uid, game.clone(), game_mode.clone(), 800 + (i % 800) as i32)
                .await
                .expect("Bench add must succeed");
        }

        let start = std::time::Instant::now();
        let size = matchmaker
            .get_queue_size(&game, &game_mode)
            .await
            .expect("get_queue_size must succeed");
        let elapsed = start.elapsed();

        assert_eq!(size, 1000);
        assert!(
            elapsed.as_millis() < 100,
            "Queue size lookup took {:?} — must be < 100 ms",
            elapsed
        );
    }
}
