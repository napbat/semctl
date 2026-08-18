//! The escalation ladder: given the segment's running counts, decide whether
//! this eligible search should fire a nudge and at which tier. Pure — no IO,
//! no clock — so it is exhaustively unit-testable.
//!
//! With defaults (`grace 1`, `cooldown 3`, `max 4`) nudges land at eligible-search
//! `n = 2` (Tier 1), then `n = 5, 8, 11` (Tier 2), and stop until the segment
//! resets. The cooldown is the steady-state anti-spam guarantee; `max = 0` means
//! unlimited (cooldown-only).

/// Eligible-search count at which tailored Tier 2 guidance begins.
const TIER2_AT: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Thresholds {
    pub grace: u32,
    pub cooldown: u32,
    /// Hard cap of nudges per segment; `0` = unlimited.
    pub max: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    One,
    Two,
}

/// Why an eligible search did not fire — surfaced so a `hook-debug` build can
/// name the specific gate an operator is tuning (grace / cooldown / cap).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SilentReason {
    Grace,
    Cap,
    Cooldown,
}

impl SilentReason {
    pub fn label(self) -> &'static str {
        match self {
            SilentReason::Grace => "grace",
            SilentReason::Cap => "cap",
            SilentReason::Cooldown => "cooldown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Silent(SilentReason),
    Fire(Tier),
}

/// Decide for the current eligible search. `eligible_count` is the count
/// **including** this call (i.e. this is the n-th eligible search this
/// segment). `last_nudge_at_count` is 0 when we have not nudged this segment.
pub fn decide(
    eligible_count: u32,
    nudges_fired: u32,
    last_nudge_at_count: u32,
    t: &Thresholds,
) -> Decision {
    // Grace: the first `grace` eligible searches are free.
    if eligible_count <= t.grace {
        return Decision::Silent(SilentReason::Grace);
    }
    // Hard cap (runaway backstop). 0 = unlimited.
    if t.max != 0 && nudges_fired >= t.max {
        return Decision::Silent(SilentReason::Cap);
    }
    // Cooldown: once nudged, wait `cooldown` eligible searches before the next.
    // `saturating_sub` so a corrupted/hand-edited state file with
    // `last_nudge_at_count > eligible_count` stays silent rather than
    // underflowing (panic in debug / wrap in release).
    if last_nudge_at_count != 0 && eligible_count.saturating_sub(last_nudge_at_count) < t.cooldown {
        return Decision::Silent(SilentReason::Cooldown);
    }
    let tier = if eligible_count >= TIER2_AT {
        Tier::Two
    } else {
        Tier::One
    };
    Decision::Fire(tier)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULTS: Thresholds = Thresholds {
        grace: 1,
        cooldown: 3,
        max: 4,
    };

    /// Replay a whole segment with the default thresholds and collect the
    /// (count, decision) at each eligible search, so the ladder is verified
    /// end-to-end rather than point-by-point.
    fn replay(n: u32, t: &Thresholds) -> Vec<(u32, Decision)> {
        let mut fired = 0;
        let mut last = 0;
        let mut out = Vec::new();
        for count in 1..=n {
            let d = decide(count, fired, last, t);
            if let Decision::Fire(_) = d {
                fired += 1;
                last = count;
            }
            out.push((count, d));
        }
        out
    }

    #[test]
    fn grace_is_silent() {
        assert_eq!(
            decide(1, 0, 0, &DEFAULTS),
            Decision::Silent(SilentReason::Grace)
        );
    }

    #[test]
    fn default_ladder_fires_at_2_5_8_11() {
        let fires: Vec<u32> = replay(30, &DEFAULTS)
            .into_iter()
            .filter(|(_, d)| matches!(d, Decision::Fire(_)))
            .map(|(c, _)| c)
            .collect();
        assert_eq!(fires, vec![2, 5, 8, 11]);
    }

    #[test]
    fn first_fire_is_tier_one_then_tier_two() {
        let decisions = replay(8, &DEFAULTS);
        assert_eq!(decisions[1], (2, Decision::Fire(Tier::One))); // n=2
        assert_eq!(decisions[4], (5, Decision::Fire(Tier::Two))); // n=5
        assert_eq!(decisions[7], (8, Decision::Fire(Tier::Two))); // n=8
    }

    #[test]
    fn cooldown_suppresses_between_fires() {
        // After firing at n=2, n=3 and n=4 are within the 3-call cooldown.
        assert_eq!(
            decide(3, 1, 2, &DEFAULTS),
            Decision::Silent(SilentReason::Cooldown)
        );
        assert_eq!(
            decide(4, 1, 2, &DEFAULTS),
            Decision::Silent(SilentReason::Cooldown)
        );
        assert_eq!(decide(5, 1, 2, &DEFAULTS), Decision::Fire(Tier::Two));
    }

    #[test]
    fn cap_and_grace_report_their_reasons() {
        let capped = Thresholds {
            grace: 1,
            cooldown: 3,
            max: 1,
        };
        // Already fired once → the cap silences and says so.
        assert_eq!(
            decide(5, 1, 2, &capped),
            Decision::Silent(SilentReason::Cap)
        );
        assert_eq!(
            decide(1, 0, 0, &capped),
            Decision::Silent(SilentReason::Grace)
        );
    }

    #[test]
    fn hard_cap_silences_after_max() {
        let capped = Thresholds {
            grace: 1,
            cooldown: 3,
            max: 2,
        };
        let fires: Vec<u32> = replay(20, &capped)
            .into_iter()
            .filter(|(_, d)| matches!(d, Decision::Fire(_)))
            .map(|(c, _)| c)
            .collect();
        assert_eq!(fires, vec![2, 5]); // stops after 2 nudges
    }

    #[test]
    fn max_zero_is_unlimited() {
        let unlimited = Thresholds {
            grace: 1,
            cooldown: 3,
            max: 0,
        };
        let fires = replay(30, &unlimited)
            .into_iter()
            .filter(|(_, d)| matches!(d, Decision::Fire(_)))
            .count();
        assert_eq!(fires, 10); // 2,5,8,…,29 — never silenced by a cap
    }

    #[test]
    fn grace_zero_fires_on_first_search() {
        let eager = Thresholds {
            grace: 0,
            cooldown: 3,
            max: 4,
        };
        assert_eq!(decide(1, 0, 0, &eager), Decision::Fire(Tier::One));
    }
}
