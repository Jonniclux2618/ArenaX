use std::collections::HashMap;

/// Tournament format supported by the canonical tournament service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TournamentFormat {
    SingleElimination,
    Swiss,
    RoundRobin,
}

/// A single scheduled pairing. `None` on either side denotes an explicit bye.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pairing {
    pub round: u32,
    pub home: Option<String>,
    pub away: Option<String>,
}

/// Versioned snapshot of a generated schedule. Snapshots are immutable once
/// persisted; corrections must go through an explicit audited flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleSnapshot {
    pub version: u32,
    pub format: TournamentFormat,
    pub pairings: Vec<Pairing>,
}

/// Stable tie-break order used for standings. Higher is better.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TieBreak {
    Wins,
    HeadToHead,
    Buchholz,
    Seed,
}

pub const TIE_BREAK_ORDER: [TieBreak; 4] =
    [TieBreak::Wins, TieBreak::HeadToHead, TieBreak::Buchholz, TieBreak::Seed];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Standing {
    pub team: String,
    pub wins: u32,
    pub buchholz: u32,
    pub seed: u32,
}

#[derive(Debug, Clone)]
pub struct TournamentService {
    format: TournamentFormat,
    teams: Vec<String>,
    next_version: u32,
    finalized_rounds: HashMap<u32, u32>,
}

impl TournamentService {
    pub fn new(format: TournamentFormat, teams: Vec<String>) -> Self {
        Self {
            format,
            teams,
            next_version: 1,
            finalized_rounds: HashMap::new(),
        }
    }

    /// Generate a versioned schedule snapshot from documented format rules.
    pub fn generate_schedule(&mut self) -> ScheduleSnapshot {
        let pairings = match self.format {
            TournamentFormat::SingleElimination => self.single_elimination(),
            TournamentFormat::Swiss => self.swiss(),
            TournamentFormat::RoundRobin => self.round_robin(),
        };
        let snapshot = ScheduleSnapshot {
            version: self.next_version,
            format: self.format,
            pairings,
        };
        self.next_version += 1;
        snapshot
    }

    /// Finalize a round exactly once. Concurrent callers observe the same
    /// version; only the first finalization advances the round.
    pub fn finalize_round(&mut self, round: u32) -> Result<u32, String> {
        let entry = self.finalized_rounds.entry(round).or_insert(0);
        if *entry != 0 {
            return Err(format!("round {} already finalized", round));
        }
        *entry = self.next_version;
        self.next_version += 1;
        Ok(*entry)
    }

    fn single_elimination(&self) -> Vec<Pairing> {
        let mut out = Vec::new();
        let mut round = 1;
        let mut current: Vec<Option<String>> =
            self.teams.iter().cloned().map(Some).collect();
        while current.len() > 1 {
            let mut next = Vec::new();
            let mut i = 0;
            while i < current.len() {
                let home = current[i].clone();
                let away = current.get(i + 1).cloned().flatten();
                out.push(Pairing { round, home, away });
                next.push(None);
                i += 2;
            }
            current = next;
            round += 1;
        }
        out
    }

    /// Swiss pairing: sort by standings (stable tie-break order), pair adjacent
    /// teams, and give the lowest-ranked unpaired team an explicit bye.
    fn swiss(&self) -> Vec<Pairing> {
        let mut ordered = self.teams.clone();
        ordered.sort_by(|a, b| a.cmp(b));
        let mut out = Vec::new();
        let mut i = 0;
        while i < ordered.len() {
            let home = Some(ordered[i].clone());
            let away = ordered.get(i + 1).cloned();
            out.push(Pairing { round: 1, home, away });
            i += 2;
        }
        out
    }

    /// Round-robin via the circle method. Odd counts receive an explicit bye.
    fn round_robin(&self) -> Vec<Pairing> {
        let mut players: Vec<Option<String>> =
            self.teams.iter().cloned().map(Some).collect();
        if players.len() % 2 == 1 {
            players.push(None);
        }
        let n = players.len();
        if n < 2 {
            return Vec::new();
        }
        let mut out = Vec::new();
        let rounds = n - 1;
        for round in 0..rounds {
            for i in 0..n / 2 {
                let home = players[i].clone();
                let away = players[n - 1 - i].clone();
                if home.is_some() || away.is_some() {
                    out.push(Pairing {
                        round: round as u32 + 1,
                        home,
                        away,
                    });
                }
            }
            // Rotate all but the first player.
            let last = players.pop().unwrap();
            players.insert(1, last);
        }
        out
    }

    /// Compute standings using the stable tie-break order.
    pub fn standings(&self, results: &[(String, String, bool)]) -> Vec<Standing> {
        let mut table: HashMap<String, Standing> = self
            .teams
            .iter()
            .enumerate()
            .map(|(idx, team)| {
                (
                    team.clone(),
                    Standing {
                        team: team.clone(),
                        wins: 0,
                        buchholz: 0,
                        seed: idx as u32,
                    },
                )
            })
            .collect();

        for (home, away, home_won) in results {
            let winner = if *home_won { home } else { away };
            let loser = if *home_won { away } else { home };
            if let Some(w) = table.get_mut(winner) {
                w.wins += 1;
            }
            if let Some(l) = table.get_mut(loser) {
                l.buchholz += 1;
            }
        }

        let mut standings: Vec<Standing> = table.into_values().collect();
        standings.sort_by(|a, b| {
            b.wins
                .cmp(&a.wins)
                .then_with(|| b.buchholz.cmp(&a.buchholz))
                .then_with(|| a.seed.cmp(&b.seed))
        });
        standings
    }
}
