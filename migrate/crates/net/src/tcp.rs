//! [`TcpLink`]: the real transport behind the [`Link`](crate::Link) trait, a TCP connection split
//! back into the same length-delimited frames [`FaultyLink`](crate::FaultyLink) moves in memory.
//! The socket is non-blocking, so `recv` returns `Ok(None)` when no whole frame has arrived yet and
//! `send` never blocks the caller: a frame the kernel can't take at once waits in an outgoing buffer
//! and drains on later calls. A reset, a peer close, or a frame whose declared length is impossible
//! cuts the link (`Err(Cut)`); the migration recovers by opening a fresh connection and resending
//! from the last acknowledgement (invariant U2), exactly as it does after a `FaultyLink` cut.
//!
//! This is transport only. It never parses a frame's contents; it just needs the wire's framing
//! rule ([`frame_total_len`]) to know where one frame ends and the next begins.

use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use bootshift_migrate_core::{MAX_FRAME, frame_total_len};

use crate::link::{Frame, Link, LinkError};

/// How much to pull off the socket per read attempt.
const READ_CHUNK: usize = 256 * 1024;

pub struct TcpLink {
    stream: TcpStream,
    /// Bytes read from the socket but not yet handed out as whole frames.
    inbuf: Vec<u8>,
    /// Bytes queued to send but not yet accepted by the kernel.
    outbuf: Vec<u8>,
    /// How far into `outbuf` the kernel has taken; the rest waits.
    out_sent: usize,
    cut: bool,
}

impl TcpLink {
    /// Dials `addr` (the sender's side).
    pub fn connect(addr: impl ToSocketAddrs) -> io::Result<Self> {
        let stream = TcpStream::connect(addr)?;
        Self::from_stream(stream)
    }

    /// Accepts one connection on `listener` (the receiver's side).
    pub fn accept(listener: &TcpListener) -> io::Result<Self> {
        let (stream, _peer) = listener.accept()?;
        Self::from_stream(stream)
    }

    pub fn from_stream(stream: TcpStream) -> io::Result<Self> {
        stream.set_nodelay(true)?;
        stream.set_nonblocking(true)?;
        Ok(Self {
            stream,
            inbuf: Vec::new(),
            outbuf: Vec::new(),
            out_sent: 0,
            cut: false,
        })
    }

    /// Blocks until everything queued has been handed to the kernel, or the link cuts. The sender
    /// calls this before it waits on the final acknowledgement, so no chunk is left unsent.
    pub fn flush(&mut self, timeout: Duration) -> Result<(), LinkError> {
        let deadline = Instant::now() + timeout;
        loop {
            self.pump_out();
            if self.cut {
                return Err(LinkError::Cut);
            }
            if self.out_sent == self.outbuf.len() {
                self.outbuf.clear();
                self.out_sent = 0;
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(LinkError::Cut);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    pub fn is_cut(&self) -> bool {
        self.cut
    }

    /// Writes as much of the outgoing buffer as the kernel will take right now.
    fn pump_out(&mut self) {
        while self.out_sent < self.outbuf.len() {
            match self.stream.write(&self.outbuf[self.out_sent..]) {
                Ok(0) => {
                    self.cut = true;
                    return;
                }
                Ok(n) => self.out_sent += n,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    self.cut = true;
                    return;
                }
            }
        }
        if self.out_sent == self.outbuf.len() && !self.outbuf.is_empty() {
            self.outbuf.clear();
            self.out_sent = 0;
        }
    }

    /// Reads whatever has arrived into the incoming buffer.
    fn pump_in(&mut self) {
        let mut chunk = [0u8; READ_CHUNK];
        loop {
            match self.stream.read(&mut chunk) {
                Ok(0) => {
                    // The peer closed. Anything already buffered is still returned; after that the
                    // link reads as cut.
                    self.cut = true;
                    return;
                }
                Ok(n) => self.inbuf.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    self.cut = true;
                    return;
                }
            }
        }
    }

    /// Splits the next whole frame off the front of the incoming buffer, if one is complete.
    fn take_frame(&mut self) -> Option<Frame> {
        let total = frame_total_len(&self.inbuf)?;
        if total > MAX_FRAME {
            // An impossible length: the stream is corrupt or desynchronised. Cut rather than try to
            // allocate for it.
            self.cut = true;
            return None;
        }
        if self.inbuf.len() < total {
            return None;
        }
        let frame = self.inbuf[..total].to_vec();
        self.inbuf.drain(..total);
        Some(frame)
    }
}

impl Link for TcpLink {
    fn send(&mut self, frame: Frame) {
        if self.cut {
            return;
        }
        self.outbuf.extend_from_slice(&frame);
        self.pump_out();
    }

    fn recv(&mut self) -> Result<Option<Frame>, LinkError> {
        self.pump_out();
        if !self.cut {
            self.pump_in();
        }
        // A frame already buffered is handed out even if the peer has since closed.
        if let Some(frame) = self.take_frame() {
            return Ok(Some(frame));
        }
        if self.cut {
            return Err(LinkError::Cut);
        }
        Ok(None)
    }
}
