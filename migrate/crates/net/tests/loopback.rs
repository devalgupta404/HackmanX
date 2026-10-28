//! `TcpLink` over a real loopback TCP connection: frames must survive being split across TCP
//! segments (a 4 MiB chunk is far larger than one segment), arrive in order and byte-identical, and
//! the link must report `Cut` once the peer closes.

#![allow(clippy::expect_used)]

use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

use bootshift_migrate_core::{Codec, DiskRange, Epoch, FrameSeq, Message, decode, encode};
use bootshift_migrate_net::{Link, LinkError, TcpLink};

const EPOCH: Epoch = Epoch(3);

fn recv_blocking(link: &mut TcpLink, timeout: Duration) -> Result<Option<Vec<u8>>, LinkError> {
    let deadline = Instant::now() + timeout;
    loop {
        match link.recv()? {
            Some(frame) => return Ok(Some(frame)),
            None => {
                if Instant::now() >= deadline {
                    return Ok(None);
                }
                thread::sleep(Duration::from_millis(1));
            }
        }
    }
}

fn big_chunk(seq: u64) -> Message {
    let data = vec![(seq & 0xff) as u8; 4 << 20];
    Message::Chunk {
        pass: 1,
        source: DiskRange::new(0, 4 << 20),
        hash: *blake3::hash(&data).as_bytes(),
        codec: Codec::None,
        data,
    }
}

#[test]
fn frames_survive_a_round_trip_including_a_large_one() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");

    // The receiver: accept, read three frames, and ack each sequence back.
    let receiver = thread::spawn(move || {
        let mut link = TcpLink::accept(&listener).expect("accept");
        let mut got = Vec::new();
        for _ in 0..3 {
            let frame = recv_blocking(&mut link, Duration::from_secs(10))
                .expect("recv")
                .expect("a frame");
            let (_, seq, message) = decode(&frame).expect("decode");
            link.send(encode(EPOCH, seq, &Message::Ack { acked_seq: seq.0 }).expect("ack"));
            got.push(message);
        }
        link.flush(Duration::from_secs(5)).expect("flush acks");
        got
    });

    let mut link = TcpLink::connect(addr).expect("connect");
    let sent = [big_chunk(1), Message::Freeze, big_chunk(2)];
    for (i, message) in sent.iter().enumerate() {
        link.send(encode(EPOCH, FrameSeq(i as u64 + 1), message).expect("encode"));
    }
    link.flush(Duration::from_secs(10)).expect("flush");

    // The three acks come back, in order.
    for i in 0..3 {
        let frame = recv_blocking(&mut link, Duration::from_secs(10))
            .expect("recv ack")
            .expect("an ack");
        let (_, _, message) = decode(&frame).expect("decode ack");
        assert_eq!(
            message,
            Message::Ack {
                acked_seq: i as u64 + 1
            }
        );
    }

    let got = receiver.join().expect("receiver thread");
    assert_eq!(got, sent, "every frame arrived byte-identical and in order");
}

#[test]
fn a_closed_peer_reads_as_cut_after_the_buffered_frame() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");

    let sender = thread::spawn(move || {
        let stream = TcpStream::connect(addr).expect("connect");
        let mut link = TcpLink::from_stream(stream).expect("wrap");
        link.send(encode(EPOCH, FrameSeq(1), &Message::Freeze).expect("encode"));
        link.flush(Duration::from_secs(5)).expect("flush");
        // Drop the link: the TCP connection closes.
    });

    let mut link = TcpLink::accept(&listener).expect("accept");
    sender.join().expect("sender thread");

    // The one buffered frame still comes through.
    let frame = recv_blocking(&mut link, Duration::from_secs(10))
        .expect("recv")
        .expect("the buffered frame");
    assert_eq!(decode(&frame).expect("decode").2, Message::Freeze);

    // After that, the closed peer reads as cut (poll a few times to let the EOF land).
    let mut saw_cut = false;
    for _ in 0..1000 {
        match link.recv() {
            Err(LinkError::Cut) => {
                saw_cut = true;
                break;
            }
            Ok(_) => thread::sleep(Duration::from_millis(1)),
        }
    }
    assert!(saw_cut, "a closed peer eventually reads as a cut link");
}
