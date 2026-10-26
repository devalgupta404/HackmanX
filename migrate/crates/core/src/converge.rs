//! The convergence model (architecture A6, implementation I5.6). After each pass it predicts the
//! data downtime a cutover would cost and advises what to do: hold ready, keep going, step up to a
//! throttle or a mirror, or report that it can't converge. Pure, so the whole decision table is
//! property-tested: it never says *ready* above budget, and it never loops for ever on a fixed
//! churn rate.

use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PassKind {
    Bulk,
    Delta,
    Compare,
    Final,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PassStats {
    pub kind: PassKind,
    pub sent_bytes: u64,
    pub duration: Duration,
    /// Bytes still dirty when the pass ended, drained for the next pass.
    pub dirty_at_end: u64,
    /// Effective rate over the pass: `min(link, read, apply)` in bytes per second.
    pub rate: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Advice {
    Continue,
    OfferMirror,
    OfferThrottle { target_rate: u64 },
    Ready { predicted_downtime: Duration },
    CannotConverge { predicted_downtime: Duration },
}

#[derive(Clone, Debug)]
pub struct ConvergenceModel {
    budget: Duration,
    /// Round trip + receiver flush + freeze; measured at rehearsal, or a default before then.
    fixed_cost: Duration,
    mirror_available: bool,
    throttled_to_floor: bool,
    history: Vec<PassStats>,
}

impl ConvergenceModel {
    pub fn new(budget: Duration, fixed_cost: Duration, mirror_available: bool) -> Self {
        ConvergenceModel {
            budget,
            fixed_cost,
            mirror_available,
            throttled_to_floor: false,
            history: Vec::new(),
        }
    }

    pub fn record(&mut self, stats: PassStats) {
        self.history.push(stats);
    }

    /// Call when a throttle has been stepped down as far as the user's floor allows, so the model
    /// stops offering it and reports can't-converge instead.
    pub fn throttle_at_floor(&mut self) {
        self.throttled_to_floor = true;
    }

    /// The predicted data downtime if the cutover froze now: the time to send what is still dirty,
    /// plus the fixed round-trip, flush and freeze cost.
    pub fn predicted_downtime(&self) -> Option<Duration> {
        let last = self.history.last()?;
        let rate = last.rate.max(1);
        Some(Duration::from_secs_f64(last.dirty_at_end as f64 / rate as f64) + self.fixed_cost)
    }

    pub fn advise(&self) -> Advice {
        let Some(predicted) = self.predicted_downtime() else {
            return Advice::Continue;
        };
        let rate = self.history.last().map_or(1, |p| p.rate.max(1));
        if predicted <= self.budget {
            return Advice::Ready {
                predicted_downtime: predicted,
            };
        }
        // Give it a couple of passes before reacting, so one write burst doesn't trigger a step-up.
        if self.history.len() < 3 || self.shrinking_over_last_two() {
            return Advice::Continue;
        }
        if self.mirror_available {
            return Advice::OfferMirror;
        }
        if !self.throttled_to_floor {
            return Advice::OfferThrottle {
                target_rate: rate / 2,
            };
        }
        Advice::CannotConverge {
            predicted_downtime: predicted,
        }
    }

    /// Whether the dirty set shrank by at least 20% over the last two passes.
    fn shrinking_over_last_two(&self) -> bool {
        let n = self.history.len();
        if n < 3 {
            return false;
        }
        let before = self.history[n - 3].dirty_at_end;
        let now = self.history[n - 1].dirty_at_end;
        before > 0 && (now as f64) <= 0.8 * (before as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn pass(dirty_at_end: u64, rate: u64) -> PassStats {
        PassStats {
            kind: PassKind::Delta,
            sent_bytes: 0,
            duration: Duration::from_secs(1),
            dirty_at_end,
            rate,
        }
    }

    #[test]
    fn ready_when_the_final_delta_fits_the_budget() {
        let mut model = ConvergenceModel::new(Duration::from_secs(5), Duration::ZERO, false);
        model.record(pass(1_000_000, 1_000_000_000)); // 1 ms to send
        assert!(matches!(model.advise(), Advice::Ready { .. }));
    }

    #[test]
    fn offers_mirror_when_stuck_and_available() {
        let mut model = ConvergenceModel::new(Duration::from_secs(1), Duration::ZERO, true);
        for _ in 0..3 {
            model.record(pass(10_000_000_000, 1_000_000)); // way above budget, not shrinking
        }
        assert_eq!(model.advise(), Advice::OfferMirror);
    }

    #[test]
    fn keeps_going_while_the_dirty_set_shrinks() {
        let mut model = ConvergenceModel::new(Duration::from_secs(1), Duration::ZERO, false);
        model.record(pass(100_000_000, 1_000));
        model.record(pass(60_000_000, 1_000));
        model.record(pass(40_000_000, 1_000)); // 40M <= 0.8 * 100M
        assert_eq!(model.advise(), Advice::Continue);
    }

    proptest! {
        /// It never claims *ready* while the predicted downtime is over budget, and on a fixed,
        /// over-budget churn it reaches a terminal decision rather than looping on Continue.
        #[test]
        fn never_ready_over_budget_and_always_terminates(
            dirty in 1u64..10_000_000_000,
            rate in 1u64..10_000_000_000,
            budget_s in 1u64..30,
        ) {
            let budget = Duration::from_secs(budget_s);
            let mut model = ConvergenceModel::new(budget, Duration::ZERO, false);
            for _ in 0..5 {
                model.record(pass(dirty, rate));
            }
            let predicted = model.predicted_downtime().unwrap_or(Duration::ZERO);
            if let Advice::Ready { .. } = model.advise() {
                prop_assert!(predicted <= budget);
            }
            // With a constant over-budget dirty set and no mirror, it must not sit on Continue.
            if predicted > budget {
                prop_assert_ne!(model.advise(), Advice::Continue);
            }
        }
    }
}
