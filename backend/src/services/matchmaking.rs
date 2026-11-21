use std::collections::HashMap;

/// Versioned, bounded calibration parameters for uncertainty-aware placement.
///
/// Parameters are versioned so historical outcomes can be evaluated against a
/// specific calibration without retroactively changing finalized results.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CalibrationParams {
    /// Version identifier for this calibration configuration.
    pub version: u32,
    /// Conservative provisional rating assigned to new accounts.
    pub provisional_rating: f64,
    /// Initial uncertainty (rating deviation) for new accounts.
    pub initial_uncertainty: f64,
    /// Minimum uncertainty floor; uncertainty never drops below this.
    pub min_uncertainty: f64,
    /// Maximum uncertainty ceiling; uncertainty never exceeds this.
    pub max_uncertainty: f64,
    /// Multiplier applied to uncertainty reduction per eligible finalized match.
    pub uncertainty_decay: f64,
}

impl CalibrationParams {
    /// Current calibration version. Bump when tuning values.
    pub const CURRENT_VERSION: u32 = 1;

    /// Bounds for each parameter, used to clamp untrusted or tuned values.
    const PROVISIONAL_RATING_RANGE: (f64, f64) = (800.0, 1600.0);
    const INITIAL_UNCERTAINTY_RANGE: (f64, f64) = (200.0, 500.0);
    const MIN_UNCERTAINTY_RANGE: (f64, f64) = (25.0, 100.0);
    const MAX_UNCERTAINTY_RANGE: (f64, f64) = (350.0, 600.0);
    const UNCERTAINTY_DECAY_RANGE: (f64, f64) = (0.5, 0.95);

    /// Construct a calibration, clamping every parameter to safe bounds.
    pub fn bounded(
        version: u32,
        provisional_rating: f64,
        initial_uncertainty: f64,
        min_uncertainty: f64,
        max_uncertainty: f64,
        uncertainty_decay: f64,
    ) -> Self {
        let min_uncertainty = clamp(min_uncertainty, Self::MIN_UNCERTAINTY_RANGE);
        let max_uncertainty = clamp(max_uncertainty, Self::MAX_UNCERTAINTY_RANGE);
        // Ensure the floor never exceeds the ceiling.
        let min_uncertainty = min_uncertainty.min(max_uncertainty);
        Self {
            version,
            provisional_rating: clamp(provisional_rating, Self::PROVISIONAL_RATING_RANGE),
            initial_uncertainty: clamp(initial_uncertainty, Self::INITIAL_UNCERTAINTY_RANGE),
            min_uncertainty,
            max_uncertainty,
            uncertainty_decay: clamp(uncertainty_decay, Self::UNCERTAINTY_DECAY_RANGE),
        }
    }

    /// Default calibration for the current version.
    pub fn current() -> Self {
        Self::bounded(Self::CURRENT_VERSION, 1200.0, 350.0, 50.0, 500.0, 0.85)
    }
}

fn clamp(value: f64, range: (f64, f64)) -> f64 {
    if value.is_nan() {
        return range.0;
    }
    value.max(range.0).min(range.1)
}

/// A player's rating state, including uncertainty.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RatingState {
    pub rating: f64,
    pub uncertainty: f64,
    /// Number of eligible finalized matches applied to this state.
    pub finalized_matches: u32,
}

impl RatingState {
    /// Conservative provisional state for a brand-new account.
    pub fn provisional(params: &CalibrationParams) -> Self {
        Self {
            rating: params.provisional_rating,
            uncertainty: params.initial_uncertainty,
            finalized_matches: 0,
        }
    }
}

/// Outcome of a finalized match from a player's perspective.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchOutcome {
    Win,
    Loss,
    Draw,
}

/// Lifecycle status of a match record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchStatus {
    /// Match is still in progress; must not affect ratings.
    InProgress,
    /// Match was finalized and is eligible for rating updates.
    Finalized,
    /// Match was voided; must not affect ratings.
    Voided,
}

/// A finalized match result relevant to rating updates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MatchResult {
    pub status: MatchStatus,
    pub outcome: MatchOutcome,
    /// Opponent rating at the time the match was finalized.
    pub opponent_rating: f64,
}

impl MatchResult {
    /// Only finalized matches are eligible to change ratings.
    pub fn is_eligible(&self) -> bool {
        matches!(self.status, MatchStatus::Finalized)
    }
}

/// Canonical rating service boundary. All placement and matchmaking rating
/// updates flow through here so uncertainty handling stays consistent.
pub struct RatingService {
    params: CalibrationParams,
}

impl RatingService {
    pub fn new(params: CalibrationParams) -> Self {
        Self { params }
    }

    pub fn params(&self) -> &CalibrationParams {
        &self.params
    }

    /// Create the conservative provisional state for a new account.
    pub fn place_new_account(&self) -> RatingState {
        RatingState::provisional(&self.params)
    }

    /// Apply a finalized match to a player's rating state.
    ///
    /// Ineligible matches (in-progress or voided) are ignored and leave the
    /// state unchanged, so uncertainty only decreases with eligible finalized
    /// matches. Finalized records themselves are never mutated here.
    pub fn apply_match(&self, state: RatingState, result: &MatchResult) -> RatingState {
        if !result.is_eligible() {
            return state;
        }

        let expected = expected_score(state.rating, result.opponent_rating);
        let actual = match result.outcome {
            MatchOutcome::Win => 1.0,
            MatchOutcome::Loss => 0.0,
            MatchOutcome::Draw => 0.5,
        };

        // Elo-style update scaled by current uncertainty: less certain players
        // move more, established players move less.
        let k = state.uncertainty;
        let new_rating = state.rating + k * (actual - expected);

        // Uncertainty decreases only on eligible finalized matches, bounded by
        // the calibration floor.
        let new_uncertainty = (state.uncertainty * self.params.uncertainty_decay)
            .max(self.params.min_uncertainty)
            .min(self.params.max_uncertainty);

        RatingState {
            rating: new_rating,
            uncertainty: new_uncertainty,
            finalized_matches: state.finalized_matches.saturating_add(1),
        }
    }

    /// Apply a batch of finalized matches in order.
    pub fn apply_matches(&self, state: RatingState, results: &[MatchResult]) -> RatingState {
        results
            .iter()
            .fold(state, |acc, result| self.apply_match(acc, result))
    }
}

fn expected_score(rating: f64, opponent_rating: f64) -> f64 {
    1.0 / (1.0 + 10f64.powf((opponent_rating - rating) / 400.0))
}

/// In-memory store used by matchmaking to look up rating states.
pub struct RatingStore {
    service: RatingService,
    states: HashMap<String, RatingState>,
}

impl RatingStore {
    pub fn new(service: RatingService) -> Self {
        Self {
            service,
            states: HashMap::new(),
        }
    }

    /// Get or create a rating state for a player. New players receive the
    /// conservative provisional placement.
    pub fn get_or_place(&mut self, player_id: &str) -> RatingState {
        if let Some(state) = self.states.get(player_id) {
            return *state;
        }
        let state = self.service.place_new_account();
        self.states.insert(player_id.to_string(), state);
        state
    }

    /// Record a finalized match for a player, updating their rating state.
    pub fn record_match(&mut self, player_id: &str, result: &MatchResult) -> RatingState {
        let current = self.get_or_place(player_id);
        let updated = self.service.apply_match(current, result);
        self.states.insert(player_id.to_string(), updated);
        updated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_account_is_conservative_and_uncertain() {
        let service = RatingService::new(CalibrationParams::current());
        let state = service.place_new_account();
        assert_eq!(state.rating, 1200.0);
        assert_eq!(state.uncertainty, 350.0);
        assert_eq!(state.finalized_matches, 0);
    }

    #[test]
    fn ineligible_matches_do_not_change_state() {
        let service = RatingService::new(CalibrationParams::current());
        let state = service.place_new_account();
        for status in [MatchStatus::InProgress, MatchStatus::Voided] {
            let result = MatchResult {
                status,
                outcome: MatchOutcome::Win,
                opponent_rating: 1200.0,
            };
            assert_eq!(service.apply_match(state, &result), state);
        }
    }

    #[test]
    fn uncertainty_decreases_only_with_finalized_matches() {
        let service = RatingService::new(CalibrationParams::current());
        let mut state = service.place_new_account();
        let result = MatchResult {
            status: MatchStatus::Finalized,
            outcome: MatchOutcome::Win,
            opponent_rating: 1200.0,
        };
        let before = state.uncertainty;
        state = service.apply_match(state, &result);
        assert!(state.uncertainty < before);
        assert_eq!(state.finalized_matches, 1);
    }

    #[test]
    fn uncertainty_is_bounded_below() {
        let service = RatingService::new(CalibrationParams::current());
        let mut state = service.place_new_account();
        let result = MatchResult {
            status: MatchStatus::Finalized,
            outcome: MatchOutcome::Loss,
            opponent_rating: 1200.0,
        };
        for _ in 0..100 {
            state = service.apply_match(state, &result);
        }
        assert!(state.uncertainty >= service.params().min_uncertainty);
    }

    #[test]
    fn calibration_params_are_clamped() {
        let params = CalibrationParams::bounded(1, 5000.0, 10.0, 5.0, 9999.0, 2.0);
        assert!(params.provisional_rating <= 1600.0);
        assert!(params.initial_uncertainty >= 200.0);
        assert!(params.min_uncertainty >= 25.0);
        assert!(params.max_uncertainty <= 600.0);
        assert!(params.uncertainty_decay <= 0.95);
        assert!(params.min_uncertainty <= params.max_uncertainty);
    }

    #[test]
    fn store_places_new_players() {
        let mut store = RatingStore::new(RatingService::new(CalibrationParams::current()));
        let state = store.get_or_place("player-1");
        assert_eq!(state.finalized_matches, 0);
        assert_eq!(state.uncertainty, 350.0);
    }
}
