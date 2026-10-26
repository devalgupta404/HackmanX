//! The [`Link`] trait and [`FaultyLink`], an in-memory connected pair whose faults are **scripted,
//! not random**, so the two-sided crash harness can drive "cut after frame N, reconnect" and "drop
//! frame N" deterministically and reproduce any failure.
//!
//! Like TCP, a link preserves order and never reorders; its faults are *drop*, *duplicate* and
//! *cut* (which loses whatever was in flight and must be followed by `reconnect`). The migration's
//! own acknowledgements and resend-on-reconnect recover from all three (invariant U2).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// An opaque, length-delimited unit. The typed protocol (`migrate-core::proto`) encodes into these;
/// the transport only moves bytes and may fault them.
pub type Frame = Vec<u8>;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LinkError {
    #[error("the link is cut; reconnect and resend from the last acknowledgement")]
    Cut,
}

/// A non-blocking, ordered, faultable byte-frame channel. One `Link` is one endpoint of a pair.
pub trait Link {
    /// Enqueues a frame for the peer. On a cut link the frame is silently lost (as a TCP send into
    /// a reset connection is), to be resent after reconnect.
    fn send(&mut self, frame: Frame);

    /// The next frame from the peer, `Ok(None)` if none is waiting yet, or `Err(Cut)` while the
    /// link is cut.
    fn recv(&mut self) -> Result<Option<Frame>, LinkError>;
}

/// Which end of the pair an endpoint is. `A` is the side that calls [`FaultyLink::pair`] first; in
/// the migrator, A is the sender and B is the receiver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    A,
    B,
}

impl Side {
    fn out(self) -> usize {
        match self {
            Side::A => 0, // A→B
            Side::B => 1, // B→A
        }
    }

    fn in_(self) -> usize {
        1 - self.out()
    }
}

/// A fault to apply to the *next* frames sent in one direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// Drop the next `count` frames.
    Drop { count: usize },
    /// Deliver the next `count` frames twice.
    Duplicate { count: usize },
}

struct Shared {
    queues: [VecDeque<Frame>; 2], // [0] = A→B, [1] = B→A
    cut: bool,
    drop_next: [usize; 2],
    dup_next: [usize; 2],
}

/// One endpoint of a [`FaultyLink`].
pub struct Endpoint {
    shared: Arc<Mutex<Shared>>,
    side: Side,
}

/// Drives the faults on a [`FaultyLink`] from the test.
pub struct Control {
    shared: Arc<Mutex<Shared>>,
}

/// An in-memory connected pair of [`Endpoint`]s with a [`Control`] handle.
pub struct FaultyLink;

impl FaultyLink {
    /// A fresh connected pair and its control handle.
    pub fn pair() -> (Endpoint, Endpoint, Control) {
        let shared = Arc::new(Mutex::new(Shared {
            queues: [VecDeque::new(), VecDeque::new()],
            cut: false,
            drop_next: [0, 0],
            dup_next: [0, 0],
        }));
        let a = Endpoint {
            shared: Arc::clone(&shared),
            side: Side::A,
        };
        let b = Endpoint {
            shared: Arc::clone(&shared),
            side: Side::B,
        };
        let control = Control {
            shared: Arc::clone(&shared),
        };
        (a, b, control)
    }
}

impl Control {
    /// Cut the link: whatever is in flight in both directions is lost, and `recv` returns `Cut`
    /// until `reconnect`.
    pub fn cut(&self) {
        if let Ok(mut shared) = self.shared.lock() {
            shared.queues[0].clear();
            shared.queues[1].clear();
            shared.cut = true;
        }
    }

    pub fn reconnect(&self) {
        if let Ok(mut shared) = self.shared.lock() {
            shared.cut = false;
        }
    }

    /// Apply a fault to the next frames sent from `side`.
    pub fn inject(&self, side: Side, fault: Fault) {
        if let Ok(mut shared) = self.shared.lock() {
            match fault {
                Fault::Drop { count } => shared.drop_next[side.out()] += count,
                Fault::Duplicate { count } => shared.dup_next[side.out()] += count,
            }
        }
    }
}

impl Endpoint {
    pub fn side(&self) -> Side {
        self.side
    }
}

impl Link for Endpoint {
    fn send(&mut self, frame: Frame) {
        let Ok(mut shared) = self.shared.lock() else {
            return;
        };
        if shared.cut {
            return; // lost on a reset connection
        }
        let out = self.side.out();
        if shared.drop_next[out] > 0 {
            shared.drop_next[out] -= 1;
            return;
        }
        let copies = if shared.dup_next[out] > 0 {
            shared.dup_next[out] -= 1;
            2
        } else {
            1
        };
        for _ in 0..copies {
            shared.queues[out].push_back(frame.clone());
        }
    }

    fn recv(&mut self) -> Result<Option<Frame>, LinkError> {
        let Ok(mut shared) = self.shared.lock() else {
            return Ok(None);
        };
        if shared.cut {
            return Err(LinkError::Cut);
        }
        let in_ = self.side.in_();
        Ok(shared.queues[in_].pop_front())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(byte: u8) -> Frame {
        vec![byte]
    }

    #[test]
    fn frames_arrive_in_order() {
        let (mut a, mut b, _c) = FaultyLink::pair();
        a.send(f(1));
        a.send(f(2));
        a.send(f(3));
        assert_eq!(b.recv(), Ok(Some(f(1))));
        assert_eq!(b.recv(), Ok(Some(f(2))));
        assert_eq!(b.recv(), Ok(Some(f(3))));
        assert_eq!(b.recv(), Ok(None));
    }

    #[test]
    fn a_dropped_frame_is_lost_the_rest_arrive() {
        let (mut a, mut b, c) = FaultyLink::pair();
        c.inject(Side::A, Fault::Drop { count: 1 });
        a.send(f(1)); // dropped
        a.send(f(2));
        assert_eq!(b.recv(), Ok(Some(f(2))));
        assert_eq!(b.recv(), Ok(None));
    }

    #[test]
    fn a_duplicated_frame_arrives_twice() {
        let (mut a, mut b, c) = FaultyLink::pair();
        c.inject(Side::A, Fault::Duplicate { count: 1 });
        a.send(f(7));
        assert_eq!(b.recv(), Ok(Some(f(7))));
        assert_eq!(b.recv(), Ok(Some(f(7))));
    }

    #[test]
    fn a_cut_loses_in_flight_and_recv_reports_it_until_reconnect() {
        let (mut a, mut b, c) = FaultyLink::pair();
        a.send(f(1)); // in flight
        c.cut();
        assert_eq!(b.recv(), Err(LinkError::Cut));
        a.send(f(2)); // lost: link is cut
        c.reconnect();
        assert_eq!(
            b.recv(),
            Ok(None),
            "the in-flight frame was lost by the cut"
        );
        a.send(f(3));
        assert_eq!(b.recv(), Ok(Some(f(3))));
    }

    #[test]
    fn the_two_directions_are_independent() {
        let (mut a, mut b, _c) = FaultyLink::pair();
        a.send(f(1));
        b.send(f(2));
        assert_eq!(a.recv(), Ok(Some(f(2))));
        assert_eq!(b.recv(), Ok(Some(f(1))));
    }
}
