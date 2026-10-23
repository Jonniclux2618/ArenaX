use crate::api_error::ApiError;
use crate::db::DbPool;
use crate::models::match_authority::*;
use crate::service::reputation_service::{ReputationError, ReputationService};
use crate::service::soroban_service::{SorobanService, SorobanTxResult, TxStatus};
use crate::transaction::{execute_transaction, IsolationLevel, TransactionConfig};
use chrono::Utc;
use sqlx::Row;
use std::sync::Arc;
use tracing::{debug, error, info, warn};
use uuid::Uuid;
use validator::Validate;

/// Match Authority Service - FSM enforcer and blockchain coordinator
pub struct MatchAuthorityService {
    db_pool: DbPool,
    soroban_service: Arc<SorobanService>,
    match_lifecycle_contract: String,
    reputation_service: Option<Arc<ReputationService>>,
}

impl MatchAuthorityService {
    /// Create a new Match Authority Service
    pub fn new(
        db_pool: DbPool,
        soroban_service: Arc<SorobanService>,
        match_lifecycle_contract: String,
    ) -> Self {
        Self {
            db_pool,
            soroban_service,
            match_lifecycle_contract,
            reputation_service: None,
        }
    }

    pub fn with_reputation_service(mut self, reputation_service: Arc<ReputationService>) -> Self {
        self.reputation_service = Some(reputation_service);
        self
    }

    // =============================================================================
    // CREATE MATCH
    // =============================================================================

    /// Create a new match (CREATED state) with transaction isolation
    /// Idempotent: same idempotency_key returns existing match
    pub async fn create_match(
        &self,
        dto: CreateMatchDTO,
        signer_secret: &str,
    ) -> Result<MatchAuthorityResponse, ApiError> {
        // Validate DTO
        validator::Validate::validate(&dto)
            .map_err(|e| ApiError::bad_request(format!("Validation error: {}", e)))?;

        let config = TransactionConfig {
            isolation_level: IsolationLevel::Serializable,
            max_retries: 3,
            ..Default::default()
        };

        let dto_clone = dto.clone();
        let signer_secret_clone = signer_secret.to_string();
        let db_pool = self.db_pool.clone();
        let soroban_service = self.soroban_service.clone();
        let match_lifecycle_contract = self.match_lifecycle_contract.clone();

        let match_id = execute_transaction(&db_pool, &config, move |tx| {
            let dto_clone = dto_clone.clone();
            let signer_secret_clone = signer_secret_clone.clone();
            let soroban_service = soroban_service.clone();
            let match_lifecycle_contract = match_lifecycle_contract.clone();
            Box::pin(async move {
                // Check for idempotency within transaction
                if let Some(ref key) = dto_clone.idempotency_key {
                    let existing = sqlx::query_as!(
                        MatchAuthorityEntity,
                        r#"
                        SELECT
                            id, on_chain_match_id, player_a, player_b, winner,
                            state as "state: MatchAuthorityState",
                            created_at, started_at, ended_at, last_chain_tx,
                            idempotency_key, metadata
                        FROM match_authority
                        WHERE idempotency_key = $1
                        FOR UPDATE
                        "#,
                        key
                    )
                    .fetch_optional(&mut **tx)
                    .await
                    .map_err(|e| ApiError::database_error(e))?;

                    if let Some(existing) = existing {
                        info!(
                            idempotency_key = key,
                            match_id = %existing.id,
                            "Returning existing match for idempotent request"
                        );
                        return Ok::<Uuid, ApiError>(existing.id);
                    }
                }

                info!(
                    player_a = %dto_clone.player_a,
                    player_b = %dto_clone.player_b,
                    "Creating new match"
                );

                // Step 1: Create match on blockchain
                let (chain_result, on_chain_match_id) = create_match_on_chain_helper(
                    &soroban_service,
                    &match_lifecycle_contract,
                    &dto_clone,
                    &signer_secret_clone,
                ).await.map_err(|e| {
                    error!(error = %e, "Failed to create match on blockchain");
                    ApiError::internal_error(format!("Blockchain creation failed: {}", e))
                })?;

                // Step 2: Create match entity in database
                let match_id = Uuid::new_v4();
                sqlx::query!(
                    r#"
                    INSERT INTO match_authority (
                        id, on_chain_match_id, player_a, player_b, state,
                        last_chain_tx, idempotency_key, metadata
                    ) VALUES (
                        $1, $2, $3, $4, 'CREATED'::match_authority_state, $5, $6, $7
                    )
                    "#,
                    match_id,
                    on_chain_match_id,
                    dto_clone.player_a,
                    dto_clone.player_b,
                    chain_result.hash.clone(),
                    dto_clone.idempotency_key,
                    serde_json::json!({})
                )
                .execute(&mut **tx)
                .await
                .map_err(|e| ApiError::database_error(e))?;

                // Step 3: Record blockchain sync
                sqlx::query!(
                    r#"
                    INSERT INTO match_chain_sync (
                        id, match_id, operation_type, tx_hash, tx_status, error_message, metadata
                    ) VALUES (
                        $1, $2, $3, $4, $5, $6, $7
                    )
                    "#,
                    Uuid::new_v4(),
                    match_id,
                    "create_match",
                    chain_result.hash,
                    "pending",
                    None::<String>,
                    serde_json::json!({})
                )
                .execute(&mut **tx)
                .await
                .map_err(|e| ApiError::database_error(e))?;

                // Step 4: Record state transition
                sqlx::query!(
                    r#"
                    INSERT INTO match_transitions (
                        id, match_id, from_state, to_state, actor, chain_tx, metadata
                    ) VALUES (
                        $1, $2, $3, $4, $5, $6, $7
                    )
                    "#,
                    Uuid::new_v4(),
                    match_id,
                    MatchAuthorityState::Created as MatchAuthorityState,
                    MatchAuthorityState::Created as MatchAuthorityState,
                    dto_clone.player_a,
                    chain_result.hash,
                    serde_json::json!({})
                )
                .execute(&mut **tx)
                .await
                .map_err(|e| ApiError::database_error(e))?;

                info!(
                    match_id = %match_id,
                    on_chain_id = %chain_result.hash,
                    "Match created successfully"
                );

                Ok(match_id)
            })
        }).await?;

        self.get_match_with_transitions(match_id).await
    }

    // =============================================================================
    // START MATCH
    // =============================================================================

    /// Start a match (CREATED -> STARTED transition) with transaction isolation
    pub async fn start_match(
        &self,
        match_id: Uuid,
        signer_secret: &str,
    ) -> Result<MatchAuthorityResponse, ApiError> {
        let config = TransactionConfig {
            isolation_level: IsolationLevel::Serializable,
            max_retries: 3,
            ..Default::default()
        };

        let match_id_clone = match_id;
        let signer_secret_clone = signer_secret.to_string();
        let db_pool = self.db_pool.clone();
        let soroban_service = self.soroban_service.clone();
        let match_lifecycle_contract = self.match_lifecycle_contract.clone();

        execute_transaction(&db_pool, &config, move |tx| {
            let signer_secret_clone = signer_secret_clone.clone();
            let soroban_service = soroban_service.clone();
            let match_lifecycle_contract = match_lifecycle_contract.clone();
            Box::pin(async move {
                // Get match with row lock
                let match_entity = sqlx::query_as!(
                    MatchAuthorityEntity,
                    r#"
                    SELECT
                        id, on_chain_match_id, player_a, player_b, winner,
                        state as "state: MatchAuthorityState",
                        created_at, started_at, ended_at, last_chain_tx,
                        idempotency_key, metadata
                    FROM match_authority
                    WHERE id = $1
                    FOR UPDATE
                    "#,
                    match_id_clone
                )
                .fetch_optional(&mut **tx)
                .await
                .map_err(|e| ApiError::database_error(e))?
                .ok_or_else(|| ApiError::not_found("Match not found"))?;

                // Validate FSM transition
                if !match_entity.state.can_transition_to(&MatchAuthorityState::Started) {
                    return Err(ApiError::bad_request(format!(
                        "Invalid state transition from {:?} to {:?}",
                        match_entity.state, MatchAuthorityState::Started
                    )));
                }

                info!(
                    match_id = %match_id_clone,
                    from_state = ?match_entity.state,
                    "Starting match"
                );

                // Step 1: Submit start_match transaction to blockchain
                let chain_result = start_match_on_chain_helper(
                    &soroban_service,
                    &match_lifecycle_contract,
                    &match_entity.on_chain_match_id,
                    &signer_secret_clone,
                ).await.map_err(|e| {
                    error!(error = %e, "Failed to start match on blockchain");
                    ApiError::internal_error(format!("Blockchain start failed: {}", e))
                })?;

                // Step 2: Update match state
                sqlx::query!(
                    r#"
                    UPDATE match_authority
                    SET state = 'STARTED'::match_authority_state,
                        last_chain_tx = $1,
                        started_at = NOW()
                    WHERE id = $2
                    "#,
                    chain_result.hash,
                    match_id_clone
                )
                .execute(&mut **tx)
                .await
                .map_err(|e| ApiError::database_error(e))?;

                // Step 3: Record blockchain sync
                sqlx::query!(
                    r#"
                    INSERT INTO match_chain_sync (
                        id, match_id, operation_type, tx_hash, tx_status, error_message, metadata
                    ) VALUES (
                        $1, $2, $3, $4, $5, $6, $7
                    )
                    "#,
                    Uuid::new_v4(),
                    match_id_clone,
                    "start_match",
                    chain_result.hash,
                    "pending",
                    None::<String>,
                    serde_json::json!({})
                )
                .execute(&mut **tx)
                .await
                .map_err(|e| ApiError::database_error(e))?;

                // Step 4: Record transition
                sqlx::query!(
                    r#"
                    INSERT INTO match_transitions (
                        id, match_id, from_state, to_state, actor, chain_tx, metadata
                    ) VALUES (
                        $1, $2, $3, $4, $5, $6, $7
                    )
                    "#,
                    Uuid::new_v4(),
                    match_id_clone,
                    match_entity.state as MatchAuthorityState,
                    MatchAuthorityState::Started as MatchAuthorityState,
                    "system",
                    chain_result.hash,
                    serde_json::json!({})
                )
                .execute(&mut **tx)
                .await
                .map_err(|e| ApiError::database_error(e))?;

                info!(match_id = %match_id_clone, "Match started successfully");

                Ok::<(), ApiError>(())
            })
        }).await?;

        self.get_match_with_transitions(match_id).await
    }

    // =============================================================================
    // COMPLETE MATCH
    // =============================================================================

    /// Complete a match (STARTED -> COMPLETED transition).
    ///
    /// # Atomicity guarantee
    ///
    /// The state check, state update, chain-sync record, and transition record
    /// are all performed inside a single serialisable transaction that acquires
    /// a `FOR UPDATE` row lock.  Concurrent calls for the same match will
    /// queue behind the lock; only one will see a valid `STARTED` state and
    /// succeed — subsequent calls will see `COMPLETED` (idempotent) or
    /// receive an `InvalidStateTransition` conflict response.
    pub async fn complete_match(
        &self,
        match_id: Uuid,
        dto: CompleteMatchDTO,
        actor: &str,
        signer_secret: &str,
    ) -> Result<MatchAuthorityResponse, ApiError> {
        // Validate DTO
        validator::Validate::validate(&dto)
            .map_err(|e| ApiError::bad_request(format!("Validation error: {}", e)))?;

        let config = TransactionConfig {
            isolation_level: IsolationLevel::Serializable,
            max_retries: 3,
            ..Default::default()
        };

        let dto_clone = dto.clone();
        let actor_owned = actor.to_string();
        let signer_secret_owned = signer_secret.to_string();
        let db_pool = self.db_pool.clone();
        let soroban_service = self.soroban_service.clone();
        let match_lifecycle_contract = self.match_lifecycle_contract.clone();

        execute_transaction(&db_pool, &config, move |tx| {
            let dto = dto_clone.clone();
            let actor = actor_owned.clone();
            let signer_secret = signer_secret_owned.clone();
            let soroban_service = soroban_service.clone();
            let match_lifecycle_contract = match_lifecycle_contract.clone();
            Box::pin(async move {
                // Acquire row lock to prevent concurrent state changes.
                let match_entity = sqlx::query_as!(
                    MatchAuthorityEntity,
                    r#"
                    SELECT
                        id, on_chain_match_id, player_a, player_b, winner,
                        state as "state: MatchAuthorityState",
                        created_at, started_at, ended_at, last_chain_tx,
                        idempotency_key, metadata
                    FROM match_authority
                    WHERE id = $1
                    FOR UPDATE
                    "#,
                    match_id
                )
                .fetch_optional(&mut **tx)
                .await
                .map_err(|e| ApiError::database_error(e))?
                .ok_or_else(|| ApiError::not_found("Match not found"))?;

                // Validate FSM transition — stable conflict response on failure.
                if !match_entity.state.can_transition_to(&MatchAuthorityState::Completed) {
                    return Err(ApiError::conflict(&format!(
                        "Invalid state transition from {:?} to Completed",
                        match_entity.state
                    )));
                }

                // Validate winner is one of the players.
                if dto.winner != match_entity.player_a && dto.winner != match_entity.player_b {
                    return Err(ApiError::bad_request(
                        "Winner must be one of the match players",
                    ));
                }

                info!(match_id = %match_id, winner = %dto.winner, "Completing match");

                let chain_result = complete_match_on_chain_helper(
                    &soroban_service,
                    &match_lifecycle_contract,
                    &match_entity.on_chain_match_id,
                    &dto.winner,
                    &signer_secret,
                )
                .await
                .map_err(|e| {
                    error!(error = %e, "Failed to complete match on blockchain");
                    ApiError::internal_error(format!("Blockchain completion failed: {}", e))
                })?;

                // Update match state within transaction.
                sqlx::query!(
                    r#"
                    UPDATE match_authority
                    SET state = 'COMPLETED'::match_authority_state,
                        winner = $1,
                        last_chain_tx = $2,
                        ended_at = NOW()
                    WHERE id = $3
                    "#,
                    dto.winner,
                    chain_result.hash,
                    match_id
                )
                .execute(&mut **tx)
                .await
                .map_err(|e| ApiError::database_error(e))?;

                // Record blockchain sync within transaction.
                sqlx::query!(
                    r#"
                    INSERT INTO match_chain_sync (
                        id, match_id, operation_type, tx_hash, tx_status, error_message, metadata
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7)
                    "#,
                    Uuid::new_v4(), match_id, "complete_match",
                    chain_result.hash, "pending", None::<String>, serde_json::json!({})
                )
                .execute(&mut **tx)
                .await
                .map_err(|e| ApiError::database_error(e))?;

                // Record transition with actor, timestamp (DB NOW()), and reason.
                sqlx::query!(
                    r#"
                    INSERT INTO match_transitions (
                        id, match_id, from_state, to_state, actor, chain_tx, metadata
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7)
                    "#,
                    Uuid::new_v4(),
                    match_id,
                    match_entity.state as MatchAuthorityState,
                    MatchAuthorityState::Completed as MatchAuthorityState,
                    actor,
                    chain_result.hash,
                    serde_json::json!({
                        "winner": dto.winner,
                        "reason": dto.reason.as_deref().unwrap_or("match_result_submitted")
                    })
                )
                .execute(&mut **tx)
                .await
                .map_err(|e| ApiError::database_error(e))?;

                info!(match_id = %match_id, "Match completed successfully");
                Ok::<(), ApiError>(())
            })
        })
        .await?;

        self.get_match_with_transitions(match_id).await
    }

    // =============================================================================
    // DISPUTE MATCH
    // =============================================================================

    /// Raise a dispute for a match (COMPLETED -> DISPUTED transition).
    ///
    /// # Atomicity guarantee
    ///
    /// The state check, state update, chain-sync record, and transition record
    /// are all performed inside a single serialisable transaction that acquires
    /// a `FOR UPDATE` row lock.  Simultaneous finalize + dispute attempts for
    /// the same match will serialize: exactly one will see `COMPLETED` and
    /// succeed; the other will see `DISPUTED` or `FINALIZED` and receive a
    /// stable conflict response without partial updates.
    pub async fn raise_dispute(
        &self,
        match_id: Uuid,
        actor: &str,
        reason: String,
        signer_secret: &str,
    ) -> Result<MatchAuthorityResponse, ApiError> {
        let config = TransactionConfig {
            isolation_level: IsolationLevel::Serializable,
            max_retries: 3,
            ..Default::default()
        };

        let actor_owned = actor.to_string();
        let reason_owned = reason.clone();
        let signer_secret_owned = signer_secret.to_string();
        let db_pool = self.db_pool.clone();
        let soroban_service = self.soroban_service.clone();
        let match_lifecycle_contract = self.match_lifecycle_contract.clone();

        execute_transaction(&db_pool, &config, move |tx| {
            let actor = actor_owned.clone();
            let reason = reason_owned.clone();
            let signer_secret = signer_secret_owned.clone();
            let soroban_service = soroban_service.clone();
            let match_lifecycle_contract = match_lifecycle_contract.clone();
            Box::pin(async move {
                // Acquire row lock to prevent concurrent state changes.
                let match_entity = sqlx::query_as!(
                    MatchAuthorityEntity,
                    r#"
                    SELECT
                        id, on_chain_match_id, player_a, player_b, winner,
                        state as "state: MatchAuthorityState",
                        created_at, started_at, ended_at, last_chain_tx,
                        idempotency_key, metadata
                    FROM match_authority
                    WHERE id = $1
                    FOR UPDATE
                    "#,
                    match_id
                )
                .fetch_optional(&mut **tx)
                .await
                .map_err(|e| ApiError::database_error(e))?
                .ok_or_else(|| ApiError::not_found("Match not found"))?;

                // Stable conflict response on invalid transition.
                if !match_entity.state.can_transition_to(&MatchAuthorityState::Disputed) {
                    return Err(ApiError::conflict(&format!(
                        "Invalid state transition from {:?} to Disputed",
                        match_entity.state
                    )));
                }

                // Only a participant may raise a dispute.
                if actor != match_entity.player_a && actor != match_entity.player_b {
                    return Err(ApiError::forbidden("Only match players can raise disputes"));
                }

                info!(match_id = %match_id, actor = %actor, "Raising dispute");

                let chain_result = raise_dispute_on_chain_helper(
                    &soroban_service,
                    &match_lifecycle_contract,
                    &match_entity.on_chain_match_id,
                    &actor,
                    &signer_secret,
                )
                .await
                .map_err(|e| {
                    error!(error = %e, "Failed to raise dispute on blockchain");
                    ApiError::internal_error(format!("Blockchain dispute failed: {}", e))
                })?;

                // Update match state within transaction.
                sqlx::query!(
                    r#"
                    UPDATE match_authority
                    SET state = 'DISPUTED'::match_authority_state,
                        last_chain_tx = $1
                    WHERE id = $2
                    "#,
                    chain_result.hash,
                    match_id
                )
                .execute(&mut **tx)
                .await
                .map_err(|e| ApiError::database_error(e))?;

                // Record blockchain sync within transaction.
                sqlx::query!(
                    r#"
                    INSERT INTO match_chain_sync (
                        id, match_id, operation_type, tx_hash, tx_status, error_message, metadata
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7)
                    "#,
                    Uuid::new_v4(), match_id, "raise_dispute",
                    chain_result.hash, "pending", None::<String>, serde_json::json!({})
                )
                .execute(&mut **tx)
                .await
                .map_err(|e| ApiError::database_error(e))?;

                // Record transition with actor, timestamp, and reason.
                sqlx::query!(
                    r#"
                    INSERT INTO match_transitions (
                        id, match_id, from_state, to_state, actor, chain_tx, metadata
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7)
                    "#,
                    Uuid::new_v4(),
                    match_id,
                    match_entity.state as MatchAuthorityState,
                    MatchAuthorityState::Disputed as MatchAuthorityState,
                    actor,
                    chain_result.hash,
                    serde_json::json!({ "reason": reason })
                )
                .execute(&mut **tx)
                .await
                .map_err(|e| ApiError::database_error(e))?;

                info!(match_id = %match_id, "Dispute raised successfully");
                Ok::<(), ApiError>(())
            })
        })
        .await?;

        self.get_match_with_transitions(match_id).await
    }

    // =============================================================================
    // FINALIZE MATCH
    // =============================================================================

    /// Finalize a match (COMPLETED/DISPUTED -> FINALIZED transition).
    /// This performs on-chain settlement.
    ///
    /// # Atomicity guarantee
    ///
    /// The state check, state update, chain-sync record, and transition record
    /// are all performed inside a single serialisable transaction that acquires
    /// a `FOR UPDATE` row lock.  Simultaneous finalize + dispute + forfeit
    /// attempts for the same match will serialize: exactly one will see a
    /// valid pre-final state and succeed — the others receive a stable
    /// conflict response without any partial row updates.
    pub async fn finalize_match(
        &self,
        match_id: Uuid,
        actor: &str,
        signer_secret: &str,
    ) -> Result<MatchAuthorityResponse, ApiError> {
        let config = TransactionConfig {
            isolation_level: IsolationLevel::Serializable,
            max_retries: 3,
            ..Default::default()
        };

        let actor_owned = actor.to_string();
        let signer_secret_owned = signer_secret.to_string();
        let db_pool = self.db_pool.clone();
        let soroban_service = self.soroban_service.clone();
        let match_lifecycle_contract = self.match_lifecycle_contract.clone();

        execute_transaction(&db_pool, &config, move |tx| {
            let actor = actor_owned.clone();
            let signer_secret = signer_secret_owned.clone();
            let soroban_service = soroban_service.clone();
            let match_lifecycle_contract = match_lifecycle_contract.clone();
            Box::pin(async move {
                // Acquire row lock — prevents concurrent finalize/dispute/forfeit.
                let match_entity = sqlx::query_as!(
                    MatchAuthorityEntity,
                    r#"
                    SELECT
                        id, on_chain_match_id, player_a, player_b, winner,
                        state as "state: MatchAuthorityState",
                        created_at, started_at, ended_at, last_chain_tx,
                        idempotency_key, metadata
                    FROM match_authority
                    WHERE id = $1
                    FOR UPDATE
                    "#,
                    match_id
                )
                .fetch_optional(&mut **tx)
                .await
                .map_err(|e| ApiError::database_error(e))?
                .ok_or_else(|| ApiError::not_found("Match not found"))?;

                // Idempotent: already finalized → succeed immediately.
                if match_entity.state == MatchAuthorityState::Finalized {
                    return Ok::<(), ApiError>(());
                }

                // Stable conflict response on invalid transition.
                if !match_entity.state.can_transition_to(&MatchAuthorityState::Finalized) {
                    return Err(ApiError::conflict(&format!(
                        "Invalid state transition from {:?} to Finalized",
                        match_entity.state
                    )));
                }

                info!(
                    match_id = %match_id,
                    from_state = ?match_entity.state,
                    actor = %actor,
                    "Finalizing match"
                );

                let chain_result = finalize_match_on_chain_helper(
                    &soroban_service,
                    &match_lifecycle_contract,
                    &match_entity.on_chain_match_id,
                    &signer_secret,
                )
                .await
                .map_err(|e| {
                    error!(error = %e, "Failed to finalize match on blockchain");
                    ApiError::internal_error(format!("Blockchain finalization failed: {}", e))
                })?;

                // Update match state within transaction.
                sqlx::query!(
                    r#"
                    UPDATE match_authority
                    SET state = 'FINALIZED'::match_authority_state,
                        last_chain_tx = $1
                    WHERE id = $2
                    "#,
                    chain_result.hash,
                    match_id
                )
                .execute(&mut **tx)
                .await
                .map_err(|e| ApiError::database_error(e))?;

                // Record blockchain sync within transaction.
                sqlx::query!(
                    r#"
                    INSERT INTO match_chain_sync (
                        id, match_id, operation_type, tx_hash, tx_status, error_message, metadata
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7)
                    "#,
                    Uuid::new_v4(), match_id, "finalize_match",
                    chain_result.hash, "pending", None::<String>, serde_json::json!({})
                )
                .execute(&mut **tx)
                .await
                .map_err(|e| ApiError::database_error(e))?;

                // Record transition with actor, timestamp, and reason.
                sqlx::query!(
                    r#"
                    INSERT INTO match_transitions (
                        id, match_id, from_state, to_state, actor, chain_tx, metadata
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7)
                    "#,
                    Uuid::new_v4(),
                    match_id,
                    match_entity.state as MatchAuthorityState,
                    MatchAuthorityState::Finalized as MatchAuthorityState,
                    actor,
                    chain_result.hash,
                    serde_json::json!({ "reason": "match_finalization" })
                )
                .execute(&mut **tx)
                .await
                .map_err(|e| ApiError::database_error(e))?;

                info!(match_id = %match_id, "Match finalized successfully");
                Ok::<(), ApiError>(())
            })
        })
        .await?;

        // Reputation update runs outside the transaction — it is best-effort
        // and must not roll back the finalization if it fails.
        if let Some(reputation_service) = &self.reputation_service {
            reputation_service
                .update_reputation_on_finalized_match(match_id)
                .await
                .map_err(|error| match error {
                    ReputationError::DatabaseError(message) => {
                        ApiError::internal_error(format!("Reputation update failed: {message}"))
                    }
                    other => ApiError::internal_error(format!("Reputation update failed: {other}")),
                })?;
        }

        self.get_match_with_transitions(match_id).await
    }

    // =============================================================================
    // QUERY METHODS
    // =============================================================================

    /// Get match by ID
    pub async fn get_match(&self, match_id: Uuid) -> Result<MatchAuthorityResponse, ApiError> {
        self.get_match_with_transitions(match_id).await
    }

    /// Return the full ordered transition history for a match.
    ///
    /// This is the primary audit and support query endpoint.  Every state
    /// change is recorded with the actor (player address or "system"), a
    /// wall-clock timestamp supplied by the database, the chain transaction
    /// hash, and a `metadata` JSON object containing the human-readable
    /// `reason` field.
    ///
    /// The result is always ordered chronologically (oldest → newest) and
    /// includes both valid transitions and any rejected attempts that were
    /// recorded with `error` set.
    pub async fn get_transition_history(
        &self,
        match_id: Uuid,
    ) -> Result<crate::models::match_authority::TransitionListResponse, ApiError> {
        // Verify the match exists before querying transitions.
        let _ = self.get_match_entity(match_id).await?;
        let transitions = self.get_match_transitions(match_id).await?;
        let total = transitions.len();
        Ok(crate::models::match_authority::TransitionListResponse {
            match_id,
            transitions,
            total,
        })
    }

    /// Get match entity (internal)
    async fn get_match_entity(&self, match_id: Uuid) -> Result<MatchAuthorityEntity, ApiError> {
        sqlx::query_as!(
            MatchAuthorityEntity,
            r#"
            SELECT
                id, on_chain_match_id, player_a, player_b, winner,
                state as "state: MatchAuthorityState",
                created_at, started_at, ended_at, last_chain_tx,
                idempotency_key, metadata
            FROM match_authority
            WHERE id = $1
            "#,
            match_id
        )
        .fetch_optional(&self.db_pool)
        .await
        .map_err(|e| ApiError::database_error(e))?
        .ok_or_else(|| ApiError::not_found("Match not found"))
    }

    /// Get match with transitions
    async fn get_match_with_transitions(
        &self,
        match_id: Uuid,
    ) -> Result<MatchAuthorityResponse, ApiError> {
        let entity = self.get_match_entity(match_id).await?;

        let transitions = self.get_match_transitions(match_id).await?;

        let mut response: MatchAuthorityResponse = entity.into();
        response.transitions = transitions;

        Ok(response)
    }

    /// Get match transitions
    async fn get_match_transitions(
        &self,
        match_id: Uuid,
    ) -> Result<Vec<MatchTransition>, ApiError> {
        sqlx::query_as!(
            MatchTransition,
            r#"
            SELECT
                id, match_id,
                from_state as "from_state: MatchAuthorityState",
                to_state as "to_state: MatchAuthorityState",
                actor, timestamp, chain_tx, metadata, error
            FROM match_transitions
            WHERE match_id = $1
            ORDER BY timestamp ASC
            "#,
            match_id
        )
        .fetch_all(&self.db_pool)
        .await
        .map_err(|e| ApiError::database_error(e))
    }

    /// Get match by idempotency key
    async fn get_match_by_idempotency_key(
        &self,
        key: &str,
    ) -> Result<Option<MatchAuthorityEntity>, ApiError> {
        sqlx::query_as!(
            MatchAuthorityEntity,
            r#"
            SELECT
                id, on_chain_match_id, player_a, player_b, winner,
                state as "state: MatchAuthorityState",
                created_at, started_at, ended_at, last_chain_tx,
                idempotency_key, metadata
            FROM match_authority
            WHERE idempotency_key = $1
            "#,
            key
        )
        .fetch_optional(&self.db_pool)
        .await
        .map_err(|e| ApiError::database_error(e))
    }

    // =============================================================================
    // RECONCILIATION
    // =============================================================================

    /// Reconcile match state with blockchain
    /// Checks if on-chain and off-chain states match
    pub async fn reconcile_match(&self, match_id: Uuid) -> Result<bool, ApiError> {
        let match_entity = self.get_match_entity(match_id).await?;

        info!(
            match_id = %match_id,
            off_chain_state = ?match_entity.state,
            "Reconciling match state"
        );

        // Fetch on-chain state
        let on_chain_state = self
            .get_match_state_from_chain(&match_entity.on_chain_match_id)
            .await
            .map_err(|e| {
                warn!(error = %e, "Failed to fetch on-chain state");
                ApiError::internal_error("Reconciliation failed")
            })?;

        let off_chain_state_str = format!("{:?}", match_entity.state);
        let is_divergent = on_chain_state != off_chain_state_str;

        // Log reconciliation
        sqlx::query!(
            r#"
            INSERT INTO match_reconciliation_log (
                id, match_id, off_chain_state, on_chain_state, is_divergent, metadata
            ) VALUES (
                $1, $2, $3, $4, $5, $6
            )
            "#,
            Uuid::new_v4(),
            match_id,
            match_entity.state as MatchAuthorityState,
            on_chain_state,
            is_divergent,
            serde_json::json!({ "checked_at": Utc::now() })
        )
        .execute(&self.db_pool)
        .await
        .map_err(|e| ApiError::database_error(e))?;

        if is_divergent {
            warn!(
                match_id = %match_id,
                off_chain = ?match_entity.state,
                on_chain = %on_chain_state,
                "State divergence detected"
            );
        } else {
            debug!(match_id = %match_id, "States are synchronized");
        }

        Ok(!is_divergent)
    }

    // =============================================================================
    // HELPER METHODS
    // =============================================================================

    /// Validate state transition according to FSM rules
    pub fn validate_transition(
        &self,
        from: &MatchAuthorityState,
        to: &MatchAuthorityState,
    ) -> Result<(), ApiError> {
        if !from.can_transition_to(to) {
            return Err(ApiError::bad_request(format!(
                "Invalid state transition from {:?} to {:?}",
                from, to
            )));
        }
        Ok(())
    }

    /// Record a state transition
    async fn record_transition(
        &self,
        match_id: Uuid,
        from: MatchAuthorityState,
        to: MatchAuthorityState,
        actor: &str,
        chain_tx: Option<&str>,
        metadata: Option<serde_json::Value>,
    ) -> Result<(), ApiError> {
        sqlx::query!(
            r#"
            INSERT INTO match_transitions (
                id, match_id, from_state, to_state, actor, chain_tx, metadata
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7
            )
            "#,
            Uuid::new_v4(),
            match_id,
            from as MatchAuthorityState,
            to as MatchAuthorityState,
            actor,
            chain_tx,
            metadata.unwrap_or_else(|| serde_json::json!({}))
        )
        .execute(&self.db_pool)
        .await
        .map_err(|e| ApiError::database_error(e))?;

        Ok(())
    }

    /// Record blockchain sync operation
    async fn record_chain_sync(
        &self,
        match_id: Uuid,
        operation_type: &str,
        tx_hash: &str,
        status: &str,
        error: Option<&str>,
    ) -> Result<(), ApiError> {
        sqlx::query!(
            r#"
            INSERT INTO match_chain_sync (
                id, match_id, operation_type, tx_hash, tx_status, error_message, metadata
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7
            )
            "#,
            Uuid::new_v4(),
            match_id,
            operation_type,
            tx_hash,
            status,
            error,
            serde_json::json!({})
        )
        .execute(&self.db_pool)
        .await
        .map_err(|e| ApiError::database_error(e))?;

        Ok(())
    }

    // =============================================================================
    // BLOCKCHAIN INTEGRATION
    // =============================================================================

    /// Create match on Soroban
    async fn create_match_on_chain(
        &self,
        dto: &CreateMatchDTO,
        signer_secret: &str,
    ) -> Result<SorobanTxResult, String> {
        let args = serde_json::json!({
            "player_a": dto.player_a,
            "player_b": dto.player_b,
        });

        self.soroban_service
            .invoke(
                &self.match_lifecycle_contract,
                "create_match",
                &args,
                signer_secret,
            )
            .await
            .map_err(|e| format!("Soroban create_match failed: {}", e))
    }

    /// Start match on Soroban
    async fn start_match_on_chain(
        &self,
        on_chain_match_id: &str,
        signer_secret: &str,
    ) -> Result<SorobanTxResult, String> {
        let args = serde_json::json!({
            "match_id": on_chain_match_id,
        });

        self.soroban_service
            .invoke(
                &self.match_lifecycle_contract,
                "start_match",
                &args,
                signer_secret,
            )
            .await
            .map_err(|e| format!("Soroban start_match failed: {}", e))
    }

    /// Complete match on Soroban
    async fn complete_match_on_chain(
        &self,
        on_chain_match_id: &str,
        winner: &str,
        signer_secret: &str,
    ) -> Result<SorobanTxResult, String> {
        let args = serde_json::json!({
            "operator": crate::service::stellar_service::stellar_public_from_secret(signer_secret)?,
            "match_id": on_chain_match_id,
            "winner": winner,
        });

        self.soroban_service
            .invoke(
                &self.match_lifecycle_contract,
                "complete_match",
                &args,
                signer_secret,
            )
            .await
            .map_err(|e| format!("Soroban complete_match failed: {}", e))
            .and_then(|result| require_successful_transaction(result, "complete_match"))
    }

    /// Raise dispute on Soroban
    async fn raise_dispute_on_chain(
        &self,
        on_chain_match_id: &str,
        actor: &str,
        signer_secret: &str,
    ) -> Result<SorobanTxResult, String> {
        let args = serde_json::json!({
            "operator": crate::service::stellar_service::stellar_public_from_secret(signer_secret)?,
            "match_id": on_chain_match_id,
            "actor": actor,
        });

        self.soroban_service
            .invoke(
                &self.match_lifecycle_contract,
                "raise_dispute_by_operator",
                &args,
                signer_secret,
            )
            .await
            .map_err(|e| format!("Soroban raise_dispute failed: {}", e))
            .and_then(|result| require_successful_transaction(result, "raise_dispute"))
    }

    /// Finalize match on Soroban (settlement)
    async fn finalize_match_on_chain(
        &self,
        on_chain_match_id: &str,
        signer_secret: &str,
    ) -> Result<SorobanTxResult, String> {
        let args = serde_json::json!({
            "operator": crate::service::stellar_service::stellar_public_from_secret(signer_secret)?,
            "match_id": on_chain_match_id,
        });

        self.soroban_service
            .invoke(
                &self.match_lifecycle_contract,
                "finalize_match",
                &args,
                signer_secret,
            )
            .await
            .map_err(|e| format!("Soroban finalize_match failed: {}", e))
            .and_then(|result| require_successful_transaction(result, "finalize_match"))
    }

    /// Get match state from blockchain
    async fn get_match_state_from_chain(&self, on_chain_match_id: &str) -> Result<String, String> {
        // In a real implementation, this would:
        // 1. Query the contract state
        // 2. Decode the response
        // 3. Return the match state

        // Placeholder for now
        debug!(
            on_chain_match_id = %on_chain_match_id,
            "Fetching match state from chain (placeholder)"
        );

        Ok("CREATED".to_string())
    }
}

// =============================================================================
// HELPER FUNCTIONS FOR TRANSACTIONS
// =============================================================================

/// Helper function to create match on chain (used inside transactions)
async fn create_match_on_chain_helper(
    soroban_service: &Arc<SorobanService>,
    match_lifecycle_contract: &str,
    dto: &CreateMatchDTO,
    signer_secret: &str,
) -> Result<(SorobanTxResult, String), String> {
    let operator = crate::service::stellar_service::stellar_public_from_secret(signer_secret)?;
    let match_id = format!(
        "{:032x}{:032x}",
        Uuid::new_v4().as_u128(),
        Uuid::new_v4().as_u128()
    );
    let args = serde_json::json!({
        "operator": operator,
        "match_id": match_id.clone(),
        "player_a": dto.player_a,
        "player_b": dto.player_b,
    });

    soroban_service
        .invoke(
            match_lifecycle_contract,
            "create_match",
            &args,
            signer_secret,
        )
        .await
        .and_then(|result| require_successful_transaction(result, "create_match"))
        .map(|result| (result, match_id))
        .map_err(|e| format!("Soroban create_match failed: {}", e))
}

/// Helper function to start match on chain (used inside transactions)
async fn start_match_on_chain_helper(
    soroban_service: &Arc<SorobanService>,
    match_lifecycle_contract: &str,
    on_chain_match_id: &str,
    signer_secret: &str,
) -> Result<SorobanTxResult, String> {
    let args = serde_json::json!({
        "actor": crate::service::stellar_service::stellar_public_from_secret(signer_secret)?,
        "match_id": on_chain_match_id,
    });

    soroban_service
        .invoke(
            match_lifecycle_contract,
            "start_match",
            &args,
            signer_secret,
        )
        .await
        .map_err(|e| format!("Soroban start_match failed: {}", e))
        .and_then(|result| require_successful_transaction(result, "start_match"))
}

fn require_successful_transaction(
    result: SorobanTxResult,
    operation: &str,
) -> Result<SorobanTxResult, String> {
    if result.status == TxStatus::Success {
        Ok(result)
    } else {
        Err(format!(
            "Soroban {operation} was not confirmed: {}",
            result.error.unwrap_or_else(|| format!("status {:?}", result.status))
        ))
    }
}

/// Helper: complete match on chain (used inside transactions).
async fn complete_match_on_chain_helper(
    soroban_service: &Arc<SorobanService>,
    match_lifecycle_contract: &str,
    on_chain_match_id: &str,
    winner: &str,
    signer_secret: &str,
) -> Result<SorobanTxResult, String> {
    let operator = crate::service::stellar_service::stellar_public_from_secret(signer_secret)?;
    let args = serde_json::json!({
        "operator": operator,
        "match_id": on_chain_match_id,
        "winner": winner,
    });
    soroban_service
        .invoke(match_lifecycle_contract, "complete_match", &args, signer_secret)
        .await
        .map_err(|e| format!("Soroban complete_match failed: {}", e))
        .and_then(|r| require_successful_transaction(r, "complete_match"))
}

/// Helper: raise dispute on chain (used inside transactions).
async fn raise_dispute_on_chain_helper(
    soroban_service: &Arc<SorobanService>,
    match_lifecycle_contract: &str,
    on_chain_match_id: &str,
    actor: &str,
    signer_secret: &str,
) -> Result<SorobanTxResult, String> {
    let operator = crate::service::stellar_service::stellar_public_from_secret(signer_secret)?;
    let args = serde_json::json!({
        "operator": operator,
        "match_id": on_chain_match_id,
        "actor": actor,
    });
    soroban_service
        .invoke(match_lifecycle_contract, "raise_dispute_by_operator", &args, signer_secret)
        .await
        .map_err(|e| format!("Soroban raise_dispute failed: {}", e))
        .and_then(|r| require_successful_transaction(r, "raise_dispute"))
}

/// Helper: finalize match on chain (used inside transactions).
async fn finalize_match_on_chain_helper(
    soroban_service: &Arc<SorobanService>,
    match_lifecycle_contract: &str,
    on_chain_match_id: &str,
    signer_secret: &str,
) -> Result<SorobanTxResult, String> {
    let operator = crate::service::stellar_service::stellar_public_from_secret(signer_secret)?;
    let args = serde_json::json!({
        "operator": operator,
        "match_id": on_chain_match_id,
    });
    soroban_service
        .invoke(match_lifecycle_contract, "finalize_match", &args, signer_secret)
        .await
        .map_err(|e| format!("Soroban finalize_match failed: {}", e))
        .and_then(|r| require_successful_transaction(r, "finalize_match"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_transition() {
        let service = MatchAuthorityService {
            db_pool: sqlx::Pool::connect_lazy("postgres://localhost/arenax").unwrap(),
            soroban_service: Arc::new(SorobanService::new(
                crate::service::soroban_service::NetworkConfig::testnet(),
            )),
            match_lifecycle_contract: "C123".to_string(),
            reputation_service: None,
        };

        // Valid transition
        assert!(service
            .validate_transition(&MatchAuthorityState::Created, &MatchAuthorityState::Started)
            .is_ok());

        // Invalid transition
        assert!(service
            .validate_transition(
                &MatchAuthorityState::Created,
                &MatchAuthorityState::Finalized
            )
            .is_err());
    }
}
