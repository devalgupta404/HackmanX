//! The pretend recovery run the restart screen plays in demo mode, power cut included. It is a
//! state machine driven by elapsed time, so the timings can be tested without a window.

use std::time::Duration;

pub const CHUNK_BYTES: u64 = 64 << 20;
const TICK: Duration = Duration::from_millis(90);
/// The run finishes in about 25 seconds, whatever the plan's size.
const TICKS_TO_FINISH: u64 = 280;
const BLACKOUT: Duration = Duration::from_millis(900);
/// When each line of the power-up story appears, after the blackout began.
const POWER_UP_LINES: [Duration; 3] = [
    BLACKOUT,
    Duration::from_millis(900 + 650),
    Duration::from_millis(900 + 650 + 650),
];
const RESUME_AFTER: Duration = Duration::from_millis(900 + 650 + 650 + 1100);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Drawn, but not playing yet.
    Ready,
    Copying,
    /// The power went out at `since`; the screen shows the first `lines` power-up lines.
    PowerCut {
        since: Duration,
        lines: usize,
    },
    Finished,
}

#[derive(Clone, Debug)]
pub struct RecoveryRun {
    pub total_bytes: u64,
    pub total_chunks: u64,
    pub chunk: u64,
    pub power_cuts: u32,
    pub phase: Phase,
    started_at: Option<Duration>,
    last_tick: Duration,
    finished_at: Option<Duration>,
}

impl RecoveryRun {
    pub fn new(total_bytes: u64) -> Self {
        let total_bytes = total_bytes.max(1);
        Self {
            total_bytes,
            total_chunks: total_bytes.div_ceil(CHUNK_BYTES).max(1),
            chunk: 0,
            power_cuts: 0,
            phase: Phase::Ready,
            started_at: None,
            last_tick: Duration::ZERO,
            finished_at: None,
        }
    }

    pub fn start(&mut self, now: Duration) {
        if self.phase == Phase::Ready {
            self.started_at = Some(now);
            self.last_tick = now;
            self.phase = Phase::Copying;
        }
    }

    pub fn cut_power(&mut self, now: Duration) {
        if self.phase == Phase::Copying {
            self.power_cuts += 1;
            self.phase = Phase::PowerCut {
                since: now,
                lines: 0,
            };
        }
    }

    /// Moves the run forward to `now`.
    pub fn advance(&mut self, now: Duration) {
        match self.phase {
            Phase::Copying => self.copy_until(now),
            Phase::PowerCut { since, .. } => {
                let elapsed = now.saturating_sub(since);
                if elapsed >= RESUME_AFTER {
                    // Nothing is lost: copying carries on from the chunk the run had reached.
                    self.phase = Phase::Copying;
                    self.last_tick = now;
                } else {
                    let lines = POWER_UP_LINES.iter().filter(|at| elapsed >= **at).count();
                    self.phase = Phase::PowerCut { since, lines };
                }
            }
            Phase::Ready | Phase::Finished => {}
        }
    }

    fn copy_until(&mut self, now: Duration) {
        let per_tick = (self.total_chunks as f64 / TICKS_TO_FINISH as f64)
            .round()
            .max(1.0) as u64;
        while now.saturating_sub(self.last_tick) >= TICK {
            self.last_tick += TICK;
            self.chunk = (self.chunk + per_tick).min(self.total_chunks);
            if self.chunk >= self.total_chunks {
                self.phase = Phase::Finished;
                self.finished_at = Some(self.last_tick);
                return;
            }
        }
    }

    pub fn share(&self) -> f64 {
        self.chunk as f64 / self.total_chunks as f64
    }

    pub fn is_playing(&self) -> bool {
        matches!(self.phase, Phase::Copying | Phase::PowerCut { .. })
    }

    /// Whole seconds from start to finish, power cuts included.
    pub fn seconds(&self) -> u64 {
        match (self.started_at, self.finished_at) {
            (Some(start), Some(end)) => end.saturating_sub(start).as_secs_f64().round() as u64,
            _ => 0,
        }
    }

    /// When the next state change is due, so the window only repaints when something moves.
    pub fn next_change(&self, now: Duration) -> Option<Duration> {
        match self.phase {
            Phase::Copying => Some((self.last_tick + TICK).saturating_sub(now)),
            Phase::PowerCut { since, lines } => {
                let due = POWER_UP_LINES.get(lines).copied().unwrap_or(RESUME_AFTER);
                Some((since + due).saturating_sub(now))
            }
            Phase::Ready | Phase::Finished => None,
        }
    }

    /// The power-up story, line by line: (is green, text).
    pub fn power_up_lines(&self) -> Vec<(bool, String)> {
        let Phase::PowerCut { lines, .. } = self.phase else {
            return Vec::new();
        };
        let story = [
            (
                false,
                "power restored. firmware hands over to BootShift recovery.".to_string(),
            ),
            (
                false,
                format!(
                    "journal: plan found. last verified chunk {} of {}.",
                    self.chunk, self.total_chunks
                ),
            ),
            (
                true,
                format!("that was rude. resuming from chunk {}.", self.chunk),
            ),
        ];
        story.into_iter().take(lines).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn chunks_are_64_mib_and_never_fewer_than_one() {
        assert_eq!(RecoveryRun::new(0).total_chunks, 1);
        assert_eq!(RecoveryRun::new(CHUNK_BYTES + 1).total_chunks, 2);
    }

    #[test]
    fn a_large_plan_finishes_in_about_25_seconds() {
        for bytes in [90u64 << 30, 2 << 40] {
            let mut run = RecoveryRun::new(bytes);
            run.start(ms(0));
            run.advance(ms(30_000));
            assert_eq!(run.phase, Phase::Finished, "{bytes}");
            assert!(
                (24..=27).contains(&run.seconds()),
                "{bytes}: {}",
                run.seconds()
            );
        }
    }

    #[test]
    fn a_small_plan_copies_one_chunk_a_tick() {
        let mut run = RecoveryRun::new(500 << 20);
        run.start(ms(0));
        run.advance(ms(8 * 90 - 1));
        assert_eq!(run.chunk, 7);
        run.advance(ms(8 * 90));
        assert_eq!(run.phase, Phase::Finished);
    }

    #[test]
    fn a_power_cut_goes_dark_then_resumes_from_the_same_chunk() {
        let mut run = RecoveryRun::new(90 << 30);
        run.start(ms(0));
        run.advance(ms(900));
        let reached = run.chunk;
        assert!(reached > 0);

        run.cut_power(ms(900));
        run.advance(ms(1_700));
        assert_eq!(run.power_up_lines().len(), 0, "dark for 900 ms");
        run.advance(ms(1_800));
        assert_eq!(run.power_up_lines().len(), 1);
        run.advance(ms(3_100));
        let lines = run.power_up_lines();
        assert_eq!(lines.len(), 3);
        assert_eq!(
            lines[1].1,
            format!("journal: plan found. last verified chunk {reached} of 1440.")
        );
        assert_eq!(run.chunk, reached, "nothing copies while the power is off");

        run.advance(ms(900 + 3_300));
        assert_eq!(run.phase, Phase::Copying);
        assert_eq!(run.chunk, reached, "resumes exactly where it was");
        assert_eq!(run.power_cuts, 1);
    }

    #[test]
    fn the_power_can_only_be_cut_while_copying() {
        let mut run = RecoveryRun::new(1);
        run.cut_power(ms(0));
        assert_eq!(run.power_cuts, 0);
        run.start(ms(0));
        run.advance(ms(100));
        assert_eq!(run.phase, Phase::Finished);
        run.cut_power(ms(100));
        assert_eq!(run.power_cuts, 0);
    }
}
