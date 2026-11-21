use std::collections::{HashMap, HashSet};

use crate::models::{Match, Party, PartyMember, QueueEntry};

/// Result of attempting to reserve a party for a match.
#[derive(Debug, PartialEq, Eq)]
pub enum ReservationOutcome {
    /// The full eligible party was reserved and a match was created.
    Matched { match_id: String },
    /// The party was rejected without any partial queue membership.
    Rejected { reason: RejectionReason },
}

#[derive(Debug, PartialEq, Eq)]
pub enum RejectionReason {
    /// Party has no eligible members (e.g. all offline or ineligible).
    NoEligibleMembers,
    /// Party size exceeds the queue's maximum group size.
    PartyTooLarge,
    /// Party leader is not present / has departed.
    LeaderMissing,
    /// A reservation for this party is already in flight.
    AlreadyReserved,
}

/// Canonical matchmaking service boundary. All queue insertion and reservation
/// flows through this struct so there is a single authoritative write path.
pub struct MatchmakingService {
    /// Active reservations keyed by party id, guarding against concurrent
    /// matchmaking creating duplicate matches for the same party.
    reservations: HashMap<String, String>,
    /// Parties currently queued, keyed by party id.
    queued: HashMap<String, Party>,
}

impl MatchmakingService {
    pub fn new() -> Self {
        Self {
            reservations: HashMap::new(),
            queued: HashMap::new(),
        }
    }

    /// Insert a party into the queue atomically. Either the full eligible party
    /// is enqueued or nothing is enqueued at all.
    pub fn enqueue_party(&mut self, party: Party, max_group_size: usize) -> Result<(), RejectionReason> {
        let eligible = Self::eligible_members(&party);

        if eligible.is_empty() {
            return Err(RejectionReason::NoEligibleMembers);
        }
        if eligible.len() > max_group_size {
            return Err(RejectionReason::PartyTooLarge);
        }
        if !Self::leader_present(&party, &eligible) {
            return Err(RejectionReason::LeaderMissing);
        }

        // Build the queued party with only eligible members so no partial or
        // ineligible membership leaks into the queue.
        let mut queued_party = party.clone();
        queued_party.members = eligible;

        self.queued.insert(queued_party.id.clone(), queued_party);
        Ok(())
    }

    /// Attempt to reserve a queued party for a match. The reservation is
    /// all-or-nothing: on success the entire party is removed from the queue
    /// and a single match is created; on failure the queue is untouched.
    pub fn try_reserve(&mut self, party_id: &str, match_id: &str) -> ReservationOutcome {
        if self.reservations.contains_key(party_id) {
            return ReservationOutcome::Rejected {
                reason: RejectionReason::AlreadyReserved,
            };
        }

        let party = match self.queued.get(party_id) {
            Some(p) => p.clone(),
            None => {
                return ReservationOutcome::Rejected {
                    reason: RejectionReason::NoEligibleMembers,
                }
            }
        };

        let eligible = Self::eligible_members(&party);
        if eligible.is_empty() {
            return ReservationOutcome::Rejected {
                reason: RejectionReason::NoEligibleMembers,
            };
        }
        if !Self::leader_present(&party, &eligible) {
            return ReservationOutcome::Rejected {
                reason: RejectionReason::LeaderMissing,
            };
        }

        // Reserve first, then remove from queue, so a concurrent caller sees
        // the reservation and cannot create a duplicate match.
        self.reservations.insert(party_id.to_string(), match_id.to_string());
        self.queued.remove(party_id);

        ReservationOutcome::Matched {
            match_id: match_id.to_string(),
        }
    }

    /// Cancel a queued party. Clears any in-flight reservation so the party is
    /// not stranded.
    pub fn cancel_party(&mut self, party_id: &str) {
        self.queued.remove(party_id);
        self.reservations.remove(party_id);
    }

    /// Handle a leader departure: the party can no longer be matched as a unit,
    /// so remove it from the queue and release any reservation.
    pub fn on_leader_departure(&mut self, party_id: &str) {
        self.cancel_party(party_id);
    }

    /// Release a reservation once a match has been finalized or expired.
    pub fn release_reservation(&mut self, party_id: &str) {
        self.reservations.remove(party_id);
    }

    fn eligible_members(party: &Party) -> Vec<PartyMember> {
        party
            .members
            .iter()
            .filter(|m| m.is_eligible())
            .cloned()
            .collect()
    }

    fn leader_present(party: &Party, eligible: &[PartyMember]) -> bool {
        let leader_id = match &party.leader_id {
            Some(id) => id,
            None => return false,
        };
        eligible.iter().any(|m| &m.user_id == leader_id)
    }
}

impl Default for MatchmakingService {
    fn default() -> Self {
        Self::new()
    }
}
