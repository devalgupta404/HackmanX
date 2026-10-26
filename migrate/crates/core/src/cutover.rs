//! The cutover fence (invariant M7, architecture A7.4). The target and the source each journal
//! their own state; neither can see the other across a dropped link. The fence is therefore
//! designed to **fail closed on the target and alive on the source**: the target boots only with
//! both its own `Committed` and the source's surrender token, and the source, until it has
//! surrendered, is always free to thaw and keep serving. A link partition can never produce two
//! live copies.
//!
//! This module is a pure model of the two state machines and the token between them, with the
//! faults that matter (a lost token, an aborted cutover). The tests walk every reachable joint
//! state and prove the two invariants, which is `plan.md` task 3.5.

/// What the target has durably recorded. It writes the target disk from `Finalizing` on, and runs
/// as the live machine from `Booted`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReceiverState {
    Receiving,
    Final,
    Committed,
    AwaitingSurrender,
    Finalizing,
    Booted,
    Verified,
    RolledBack,
}

/// What the source has durably recorded. It is the live, writable machine in `Syncing` and again
/// in `Thawed`; frozen (not writing) in `Quiescing`/`FinalPass`; and powered off in `Surrendered`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SenderState {
    Syncing,
    Quiescing,
    FinalPass,
    Committed,
    Surrendered,
    RollingBack,
    Thawed,
}

/// A snapshot of the whole cutover.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Fence {
    pub sender: SenderState,
    pub receiver: ReceiverState,
    /// The source's `SourceSurrendered(epoch)` token: emitted when the sender powers off, consumed
    /// by the receiver to leave `AwaitingSurrender`. `true` while in flight.
    pub token_in_flight: bool,
}

/// The source is a live, writable copy.
fn source_writing(state: SenderState) -> bool {
    matches!(state, SenderState::Syncing | SenderState::Thawed)
}

/// The target is being written as the live machine (its disk is mutated, or its OS is running).
fn target_writing(state: ReceiverState) -> bool {
    matches!(
        state,
        ReceiverState::Finalizing | ReceiverState::Booted | ReceiverState::Verified
    )
}

/// The source is proven to be down (powered off / fenced), so the operator may let a committed
/// target boot even if the token was lost on the link.
fn source_down(state: SenderState) -> bool {
    state == SenderState::Surrendered
}

impl Fence {
    pub fn start() -> Self {
        Fence {
            sender: SenderState::Syncing,
            receiver: ReceiverState::Receiving,
            token_in_flight: false,
        }
    }

    /// The forbidden state: both machines live and writable at once.
    pub fn two_live_copies(self) -> bool {
        source_writing(self.sender) && target_writing(self.receiver)
    }

    /// A safe outcome: the target is fully verified and live, or the source is serving (the
    /// cutover was abandoned and the untouched source kept running).
    pub fn resolved(self) -> bool {
        self.receiver == ReceiverState::Verified || self.sender == SenderState::Thawed
    }

    fn with_sender(self, sender: SenderState) -> Self {
        Fence { sender, ..self }
    }

    fn with_receiver(self, receiver: ReceiverState) -> Self {
        Fence { receiver, ..self }
    }

    /// Every joint state reachable in one step: either side advances, the source emits or the link
    /// drops the token, and a cutover may be abandoned before the source surrenders.
    pub fn successors(self) -> Vec<Fence> {
        use ReceiverState as R;
        use SenderState as S;
        let mut out = Vec::new();

        // --- sender transitions ---
        match self.sender {
            S::Syncing => out.push(self.with_sender(S::Quiescing)),
            // Watchdog / link drop before the source has surrendered: thaw and keep serving.
            S::Quiescing => {
                out.push(self.with_sender(S::FinalPass));
                out.push(self.with_sender(S::Thawed));
            }
            S::FinalPass => {
                out.push(self.with_sender(S::Committed));
                out.push(self.with_sender(S::Thawed));
            }
            S::Committed => {
                if self.receiver == R::RolledBack {
                    // The target rolled back: the source must thaw and keep serving, never
                    // surrender — otherwise the source is off and the target refuses to boot,
                    // stranding both (A7.3).
                    out.push(self.with_sender(S::RollingBack));
                } else {
                    // Happy path: power off and emit the surrender token.
                    out.push(Fence {
                        sender: S::Surrendered,
                        token_in_flight: true,
                        ..self
                    });
                }
            }
            S::RollingBack => out.push(self.with_sender(S::Thawed)),
            S::Surrendered | S::Thawed => {}
        }

        // --- receiver transitions ---
        // Rollback is only meaningful while the source can still thaw; once it has surrendered,
        // rolling back would strand both sides (outage), so it is not offered then.
        let may_roll_back = !source_down(self.sender);
        match self.receiver {
            R::Receiving => out.push(self.with_receiver(R::Final)),
            R::Final => {
                out.push(self.with_receiver(R::Committed)); // roots matched
                if may_roll_back {
                    out.push(self.with_receiver(R::RolledBack)); // roots mismatched
                }
            }
            R::Committed => {
                out.push(self.with_receiver(R::AwaitingSurrender));
                if may_roll_back {
                    out.push(self.with_receiver(R::RolledBack));
                }
            }
            R::AwaitingSurrender => {
                // The fence: proceed only with the token, or with the source proven down.
                if self.token_in_flight {
                    out.push(Fence {
                        receiver: R::Finalizing,
                        token_in_flight: false,
                        ..self
                    });
                } else if source_down(self.sender) {
                    out.push(self.with_receiver(R::Finalizing));
                }
                if may_roll_back {
                    out.push(self.with_receiver(R::RolledBack));
                }
            }
            R::Finalizing => out.push(self.with_receiver(R::Booted)),
            R::Booted => out.push(self.with_receiver(R::Verified)),
            R::Verified | R::RolledBack => {}
        }

        // --- the link drops the token before the receiver consumes it ---
        if self.token_in_flight {
            out.push(Fence {
                token_in_flight: false,
                ..self
            });
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// Every joint state reachable from the start.
    fn reachable() -> HashSet<Fence> {
        let mut seen = HashSet::new();
        let mut stack = vec![Fence::start()];
        while let Some(fence) = stack.pop() {
            if seen.insert(fence) {
                stack.extend(fence.successors());
            }
        }
        seen
    }

    #[test]
    fn two_live_copies_is_never_reachable() {
        let states = reachable();
        assert!(!states.is_empty());
        for fence in &states {
            assert!(
                !fence.two_live_copies(),
                "reachable split-brain state: {fence:?}"
            );
        }
    }

    #[test]
    fn every_reachable_state_can_reach_a_safe_outcome() {
        let states = reachable();
        // Backward fixpoint: a state is safe if it is resolved or any successor is safe.
        let mut safe: HashSet<Fence> = states.iter().copied().filter(|f| f.resolved()).collect();
        loop {
            let mut grew = false;
            for fence in &states {
                if safe.contains(fence) {
                    continue;
                }
                if fence.successors().iter().any(|s| safe.contains(s)) {
                    safe.insert(*fence);
                    grew = true;
                }
            }
            if !grew {
                break;
            }
        }
        for fence in &states {
            assert!(
                safe.contains(fence),
                "no path to a safe outcome from {fence:?}"
            );
        }
    }

    #[test]
    fn a_committed_target_cannot_boot_without_the_token_while_the_source_still_serves() {
        // Receiver committed and waiting; source aborted to Thawed (link dropped before surrender).
        let stuck = Fence {
            sender: SenderState::Thawed,
            receiver: ReceiverState::AwaitingSurrender,
            token_in_flight: false,
        };
        // It must not be able to reach Finalizing/Booted: no token, source not surrendered.
        for next in stuck.successors() {
            assert_ne!(next.receiver, ReceiverState::Finalizing);
        }
        // And it is safe: the source is serving.
        assert!(stuck.resolved());
    }
}
