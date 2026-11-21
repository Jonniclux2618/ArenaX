//! Tournament seeding service.
//!
//! Seeding is reproducible from a recorded input snapshot: the exact source
//! ratings, eligibility set, manual overrides, and algorithm version are
//! persisted alongside the resulting placement. Re-running with the same
//! snapshot yields identical placement and never mutates a started bracket.

use std::collections::BTreeMap;

/// Version of the seeding algorithm. Bump when placement logic changes so
/// recorded snapshots remain explainable.
pub const SEEDING_ALGORITHM_VERSION: &str = "seeding-v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedError {
    pub message: String,
}

impl SeedError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// A single participant's source rating at snapshot time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRating {
    pub participant_id: String,
    pub rating: i64,
}

/// An authorized manual override applied during seeding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedOverride {
    pub participant_id: String,
    /// Requested seed position (1-based).
    pub seed: u32,
    /// Identity of the operator who authorized the override.
    pub authorized_by: String,
}

/// The exact input set used to produce a seed. Persisted verbatim so a seed
/// can be explained or rebuilt later even if ratings or overrides change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedingSnapshot {
    pub algorithm_version: String,
    /// Eligible participants captured at snapshot time.
    pub eligibility: Vec<String>,
    /// Source ratings captured at snapshot time.
    pub ratings: Vec<SourceRating>,
    /// Authorized manual overrides captured at snapshot time.
    pub overrides: Vec<SeedOverride>,
}

/// A single placement entry in the seed output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedPlacement {
    pub participant_id: String,
    pub seed: u32,
    /// Deterministic tie-break key used to order this participant.
    pub tie_break_key: String,
    /// Whether this placement came from an authorized override.
    pub overridden: bool,
}

/// The full, auditable seed output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedOutput {
    pub algorithm_version: String,
    pub eligibility: Vec<String>,
    pub overrides: Vec<SeedOverride>,
    pub placements: Vec<SeedPlacement>,
}

/// Build a deterministic tie-break key from rating and participant id.
/// Higher rating sorts first; participant id breaks ties lexicographically.
fn tie_break_key(rating: i64, participant_id: &str) -> String {
    format!("{:020}:{}", rating, participant_id)
}

/// Produce a seed from a recorded snapshot.
///
/// The output is a pure function of the snapshot: identical snapshots always
/// produce identical placement. This function never mutates bracket state.
pub fn seed_from_snapshot(snapshot: &SeedingSnapshot) -> Result<SeedOutput, SeedError> {
    if snapshot.algorithm_version != SEEDING_ALGORITHM_VERSION {
        return Err(SeedError::new(format!(
            "unsupported seeding algorithm version: {}",
            snapshot.algorithm_version
        )));
    }

    let eligible: BTreeMap<&str, ()> = snapshot
        .eligibility
        .iter()
        .map(|id| (id.as_str(), ()))
        .collect();

    let mut ratings: BTreeMap<&str, i64> = BTreeMap::new();
    for rating in &snapshot.ratings {
        if !eligible.contains_key(rating.participant_id.as_str()) {
            return Err(SeedError::new(format!(
                "rating for ineligible participant: {}",
                rating.participant_id
            )));
        }
        if ratings
            .insert(rating.participant_id.as_str(), rating.rating)
            .is_some()
        {
            return Err(SeedError::new(format!(
                "duplicate rating for participant: {}",
                rating.participant_id
            )));
        }
    }

    for id in &snapshot.eligibility {
        if !ratings.contains_key(id.as_str()) {
            return Err(SeedError::new(format!(
                "missing rating for eligible participant: {}",
                id
            )));
        }
    }

    // Validate overrides: authorized, eligible, and non-conflicting.
    let mut override_seeds: BTreeMap<u32, &str> = BTreeMap::new();
    let mut overridden: BTreeMap<&str, u32> = BTreeMap::new();
    for ov in &snapshot.overrides {
        if ov.authorized_by.trim().is_empty() {
            return Err(SeedError::new(format!(
                "override for {} is not authorized",
                ov.participant_id
            )));
        }
        if !eligible.contains_key(ov.participant_id.as_str()) {
            return Err(SeedError::new(format!(
                "override for ineligible participant: {}",
                ov.participant_id
            )));
        }
        if ov.seed == 0 {
            return Err(SeedError::new("override seed must be >= 1"));
        }
        if override_seeds
            .insert(ov.seed, ov.participant_id.as_str())
            .is_some()
        {
            return Err(SeedError::new(format!(
                "conflicting override for seed {}",
                ov.seed
            )));
        }
        if overridden
            .insert(ov.participant_id.as_str(), ov.seed)
            .is_some()
        {
            return Err(SeedError::new(format!(
                "duplicate override for participant: {}",
                ov.participant_id
            )));
        }
    }

    // Order remaining participants deterministically by rating desc, id asc.
    let mut ordered: Vec<(&str, i64)> = snapshot
        .eligibility
        .iter()
        .filter(|id| !overridden.contains_key(id.as_str()))
        .map(|id| (id.as_str(), ratings[id.as_str()]))
        .collect();
    ordered.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| a.0.cmp(b.0))
    });

    let total = snapshot.eligibility.len() as u32;
    let mut placements: Vec<SeedPlacement> = Vec::with_capacity(total as usize);
    let mut next_seed: u32 = 1;
    let mut remaining = ordered.into_iter().peekable();

    while placements.len() < total as usize {
        if let Some(participant_id) = override_seeds.get(&next_seed) {
            let rating = ratings[*participant_id];
            placements.push(SeedPlacement {
                participant_id: (*participant_id).to_string(),
                seed: next_seed,
                tie_break_key: tie_break_key(rating, participant_id),
                overridden: true,
            });
            next_seed += 1;
            continue;
        }
        match remaining.next() {
            Some((participant_id, rating)) => {
                placements.push(SeedPlacement {
                    participant_id: participant_id.to_string(),
                    seed: next_seed,
                    tie_break_key: tie_break_key(rating, participant_id),
                    overridden: false,
                });
                next_seed += 1;
            }
            None => break,
        }
    }

    Ok(SeedOutput {
        algorithm_version: snapshot.algorithm_version.clone(),
        eligibility: snapshot.eligibility.clone(),
        overrides: snapshot.overrides.clone(),
        placements,
    })
}

/// Render a human-readable seed audit report from a snapshot and its output.
pub fn render_seed_audit_report(snapshot: &SeedingSnapshot, output: &SeedOutput) -> String {
    let mut report = String::new();
    report.push_str(&format!("Seeding audit report\n"));
    report.push_str(&format!("Algorithm version: {}\n", output.algorithm_version));
    report.push_str(&format!("Eligible participants: {}\n", output.eligibility.len()));
    report.push_str(&format!("Authorized overrides: {}\n", output.overrides.len()));
    report.push_str("Placements:\n");
    for placement in &output.placements {
        report.push_str(&format!(
            "  {}. {} (tie-break: {}{})\n",
            placement.seed,
            placement.participant_id,
            placement.tie_break_key,
            if placement.overridden { ", override" } else { "" }
        ));
    }
    let _ = snapshot;
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> SeedingSnapshot {
        SeedingSnapshot {
            algorithm_version: SEEDING_ALGORITHM_VERSION.to_string(),
            eligibility: vec!["a".into(), "b".into(), "c".into()],
            ratings: vec![
                SourceRating { participant_id: "a".into(), rating: 1500 },
                SourceRating { participant_id: "b".into(), rating: 1600 },
                SourceRating { participant_id: "c".into(), rating: 1500 },
            ],
            overrides: vec![],
        }
    }

    #[test]
    fn deterministic_placement() {
        let snap = snapshot();
        let first = seed_from_snapshot(&snap).unwrap();
        let second = seed_from_snapshot(&snap).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.placements[0].participant_id, "b");
        assert_eq!(first.placements[1].participant_id, "a");
        assert_eq!(first.placements[2].participant_id, "c");
    }

    #[test]
    fn rejects_unauthorized_override() {
        let mut snap = snapshot();
        snap.overrides.push(SeedOverride {
            participant_id: "c".into(),
            seed: 1,
            authorized_by: "".into(),
        });
        assert!(seed_from_snapshot(&snap).is_err());
    }

    #[test]
    fn applies_authorized_override() {
        let mut snap = snapshot();
        snap.overrides.push(SeedOverride {
            participant_id: "c".into(),
            seed: 1,
            authorized_by: "admin".into(),
        });
        let out = seed_from_snapshot(&snap).unwrap();
        assert_eq!(out.placements[0].participant_id, "c");
        assert!(out.placements[0].overridden);
    }

    #[test]
    fn rejects_unknown_version() {
        let mut snap = snapshot();
        snap.algorithm_version = "seeding-v0".into();
        assert!(seed_from_snapshot(&snap).is_err());
    }
}
