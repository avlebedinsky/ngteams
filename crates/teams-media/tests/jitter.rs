//! The jitter buffer: packets in any order come out in media-time order once it is deep enough; a missing packet is
//! recovered from the next one's FEC or concealed; late, duplicate and overflowing packets are dropped and counted; a
//! long gap stops playout until the buffer fills up again; a backlog is shed so the delay stays near the depth; a jump
//! far back in media time (a new source) starts a new epoch.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::sync::Arc;

use teams_media::codec::PACKET_TICKS;
use teams_media::jitter::{JitterBuffer, JitterConfig, JitterStats, Playout, Pushed};

const STEP: usize = 960;

/// A payload naming its packet, so a test can tell which one came out.
fn payload(index: u64) -> Arc<[u8]> {
    Arc::from(index.to_be_bytes().as_slice())
}

fn at(index: u64) -> u64 {
    // a media time that doesn't start at 0, as a real sender's doesn't
    1_000_000 + index * PACKET_TICKS
}

fn push(buffer: &mut JitterBuffer, index: u64) -> Pushed {
    buffer.push(at(index), PACKET_TICKS, payload(index))
}

fn buffer(depth: usize) -> JitterBuffer {
    JitterBuffer::new(JitterConfig {
        depth,
        capacity: 8,
        max_concealed: 3,
    })
}

fn packet(index: u64) -> Playout {
    Playout::Packet(payload(index))
}

fn recover(next: u64) -> Playout {
    Playout::Recover {
        next: payload(next),
        samples: STEP,
    }
}

const CONCEAL: Playout = Playout::Conceal { samples: STEP };

#[test]
fn playout_starts_once_the_buffer_is_deep_enough() {
    let mut jitter = buffer(3);
    assert_eq!(jitter.pop(), Playout::Silence, "nothing yet");
    push(&mut jitter, 0);
    push(&mut jitter, 1);
    assert_eq!(jitter.pop(), Playout::Silence, "two of three");
    push(&mut jitter, 2);
    assert_eq!(jitter.pop(), packet(0));
    assert_eq!(jitter.pop(), packet(1));
    assert_eq!(jitter.pop(), packet(2));
    assert!(jitter.is_empty());
}

#[test]
fn reordered_packets_come_out_in_media_time_order() {
    let mut jitter = buffer(5);
    for index in [1, 0, 3, 2, 4] {
        assert_eq!(push(&mut jitter, index), Pushed::Held);
    }
    let out: Vec<Playout> = (0..5).map(|_| jitter.pop()).collect();
    assert_eq!(out, (0..5).map(packet).collect::<Vec<_>>());
    assert_eq!(jitter.stats().played, 5);
}

#[test]
fn a_missing_packet_is_recovered_from_the_next_one_which_still_plays() {
    let mut jitter = buffer(2);
    for index in [0, 2, 3] {
        push(&mut jitter, index);
    }
    assert_eq!(jitter.pop(), packet(0));
    assert_eq!(jitter.pop(), recover(2), "1 is missing, 2 is here: its FEC");
    assert_eq!(jitter.pop(), packet(2), "then 2 itself");
    assert_eq!(jitter.pop(), packet(3));
    let stats = jitter.stats();
    assert_eq!((stats.played, stats.recovered, stats.concealed), (3, 1, 0));
}

#[test]
fn a_missing_packet_with_nothing_after_it_is_concealed_and_arrives_late() {
    let mut jitter = buffer(2);
    for index in [0, 1, 4] {
        push(&mut jitter, index);
    }
    assert_eq!(jitter.pop(), packet(0));
    assert_eq!(jitter.pop(), packet(1));
    assert_eq!(jitter.pop(), CONCEAL, "2 is missing and 3 too");
    assert_eq!(jitter.pop(), recover(4), "3 is missing, 4 is here");
    assert_eq!(push(&mut jitter, 2), Pushed::Late, "2's time was concealed");
    assert_eq!(push(&mut jitter, 3), Pushed::Late, "3's time was recovered");
    assert_eq!(jitter.pop(), packet(4));
    let stats = jitter.stats();
    assert_eq!((stats.concealed, stats.recovered, stats.late), (1, 1, 2));
}

#[test]
fn a_duplicate_is_dropped() {
    let mut jitter = buffer(1);
    assert_eq!(push(&mut jitter, 0), Pushed::Held);
    assert_eq!(push(&mut jitter, 0), Pushed::Duplicate);
    assert_eq!(jitter.pop(), packet(0));
    assert_eq!(push(&mut jitter, 0), Pushed::Late, "played already");
    assert_eq!(jitter.stats().duplicate, 1);
}

#[test]
fn a_full_buffer_drops_its_oldest_packet() {
    let mut jitter = buffer(3);
    for index in 0..8 {
        assert_eq!(push(&mut jitter, index), Pushed::Held);
    }
    assert_eq!(push(&mut jitter, 8), Pushed::Overflowed);
    assert_eq!(jitter.len(), 8, "never more than the capacity");
    assert_eq!(jitter.stats().overflowed, 1, "0 was dropped");
}

#[test]
fn a_reader_that_stalls_catches_up_without_playing_the_backlog() {
    // the speaker stalls while packets keep coming: once it plays again, playout picks up within the depth of the
    // newest packet, not at the oldest one held
    let mut jitter = buffer(2);
    push(&mut jitter, 0);
    push(&mut jitter, 1);
    assert_eq!(jitter.pop(), packet(0));
    for index in 2..20 {
        push(&mut jitter, index);
    }
    assert_eq!(jitter.len(), 8, "12 to 19");
    assert_eq!(jitter.pop(), packet(18), "the newest two are the depth");
    assert_eq!(jitter.pop(), packet(19));
    push(&mut jitter, 20);
    push(&mut jitter, 21);
    assert_eq!(jitter.pop(), packet(20), "and it keeps the pace from there");
    let stats = jitter.stats();
    assert_eq!((stats.overflowed, stats.shed, stats.concealed), (11, 6, 0), "{stats:?}");
}

#[test]
fn a_reader_that_stalls_before_playout_starts_begins_at_the_newest() {
    let mut jitter = buffer(3);
    for index in 0..8 {
        push(&mut jitter, index);
    }
    assert_eq!(
        jitter.pop(),
        packet(5),
        "8 held is more than twice the depth: the oldest five are shed"
    );
    assert_eq!(jitter.len(), 2);
    assert_eq!(jitter.stats().shed, 5);
}

#[test]
fn a_burst_while_playing_is_shed_down_to_the_depth() {
    // packets that pile up (a burst after an outage, a far clock running fast) don't add delay for good
    let mut jitter = buffer(2);
    push(&mut jitter, 0);
    push(&mut jitter, 1);
    assert_eq!(jitter.pop(), packet(0));
    for index in 2..5 {
        push(&mut jitter, index);
    }
    assert_eq!(jitter.pop(), packet(1), "four held is not more than twice the depth");
    push(&mut jitter, 5);
    push(&mut jitter, 6);
    assert_eq!(jitter.len(), 5);
    assert_eq!(
        jitter.pop(),
        packet(5),
        "more than twice the depth: shed to the newest two"
    );
    assert_eq!(jitter.pop(), packet(6));
    assert_eq!(push(&mut jitter, 4), Pushed::Late, "what was shed stays behind");
    assert_eq!(jitter.stats().shed, 3);
}

#[test]
fn a_stale_packet_before_a_pause_is_dropped_when_playout_starts_again() {
    // after a pause (DTX), playout starts with the speech, not with the last packet before the gap
    let mut jitter = buffer(2);
    push(&mut jitter, 0);
    push(&mut jitter, 1);
    assert_eq!(jitter.pop(), packet(0));
    assert_eq!(jitter.pop(), packet(1));
    for _ in 0..3 {
        assert_eq!(jitter.pop(), CONCEAL);
    }
    assert_eq!(jitter.pop(), Playout::Silence, "concealment ends");
    push(&mut jitter, 10);
    assert_eq!(jitter.pop(), Playout::Silence);
    push(&mut jitter, 40);
    assert_eq!(
        jitter.pop(),
        Playout::Silence,
        "10 is stranded before a gap playout wouldn't conceal; 40 alone is short of the depth"
    );
    push(&mut jitter, 41);
    assert_eq!(jitter.pop(), packet(40));
    assert_eq!(jitter.pop(), packet(41));
    assert_eq!(jitter.stats().shed, 1);
}

#[test]
fn a_gap_playout_conceals_keeps_what_is_before_it() {
    let mut jitter = buffer(2);
    // four missing: three concealed and one recovered bridge them
    push(&mut jitter, 0);
    push(&mut jitter, 5);
    assert_eq!(jitter.pop(), packet(0));
    assert_eq!(jitter.stats().shed, 0);
}

#[test]
fn a_jump_far_back_in_media_time_starts_a_new_epoch() {
    // a far end that changes its source starts its media time anew, here 10 s before the old one
    let mut jitter = buffer(2);
    for index in 0..3 {
        push(&mut jitter, index);
    }
    assert_eq!(jitter.pop(), packet(0));
    assert_eq!(jitter.pop(), packet(1));
    let back = |index: u64| at(index) - 480_000;
    assert_eq!(jitter.push(back(0), PACKET_TICKS, payload(100)), Pushed::NewEpoch);
    assert_eq!(jitter.len(), 1, "2 went with the old epoch");
    assert_eq!(jitter.pop(), Playout::Silence, "filling up again");
    assert_eq!(jitter.push(back(1), PACKET_TICKS, payload(101)), Pushed::Held);
    assert_eq!(jitter.pop(), packet(100));
    assert_eq!(jitter.pop(), packet(101));
    let stats = jitter.stats();
    assert_eq!((stats.epochs, stats.shed, stats.late), (1, 1, 0), "{stats:?}");
}

#[test]
fn a_packet_less_than_the_epoch_late_is_only_late() {
    let mut jitter = buffer(1);
    for index in 0..60 {
        push(&mut jitter, index);
        assert_eq!(jitter.pop(), packet(index));
    }
    // 1.2 s behind the cursor is a new epoch; 0.98 s is a late packet of the same source
    assert_eq!(push(&mut jitter, 11), Pushed::Late);
    assert_eq!(push(&mut jitter, 0), Pushed::NewEpoch);
    assert_eq!(jitter.stats().epochs, 1);
}

#[test]
fn a_silent_sender_stops_playout_until_the_buffer_fills_up_again() {
    let mut jitter = buffer(2);
    push(&mut jitter, 0);
    push(&mut jitter, 1);
    assert_eq!(jitter.pop(), packet(0));
    assert_eq!(jitter.pop(), packet(1));
    let concealed: Vec<Playout> = (0..3).map(|_| jitter.pop()).collect();
    assert_eq!(concealed, [CONCEAL, CONCEAL, CONCEAL]);
    assert_eq!(jitter.pop(), Playout::Silence, "concealment ends");
    assert_eq!(jitter.pop(), Playout::Silence, "and stays silent");
    // the sender resumes later in media time (DTX, or a pause): one packet isn't enough, two are
    push(&mut jitter, 50);
    assert_eq!(jitter.pop(), Playout::Silence);
    push(&mut jitter, 51);
    assert_eq!(jitter.pop(), packet(50));
    assert_eq!(push(&mut jitter, 3), Pushed::Late, "what was concealed stays behind");
}

#[test]
fn a_missing_packet_lasts_as_long_as_the_last_one_played() {
    let mut jitter = buffer(1);
    // 10 ms packets
    jitter.push(0, 480, payload(0));
    jitter.push(960, 480, payload(2));
    assert_eq!(jitter.pop(), packet(0));
    assert_eq!(
        jitter.pop(),
        Playout::Recover {
            next: payload(2),
            samples: 480
        },
        "the 10 ms at 480 is missing; the packet at 960 follows it"
    );
    assert_eq!(jitter.pop(), packet(2));
    assert_eq!(jitter.pop(), Playout::Conceal { samples: 480 });
}

#[test]
fn a_zero_sized_config_still_plays() {
    let mut jitter = JitterBuffer::new(JitterConfig {
        depth: 0,
        capacity: 0,
        max_concealed: 0,
    });
    push(&mut jitter, 0);
    assert_eq!(jitter.pop(), packet(0));
    assert_eq!(jitter.pop(), Playout::Silence, "no concealment allowed");
    assert_eq!(
        jitter.stats(),
        JitterStats {
            held: 1,
            played: 1,
            ..JitterStats::default()
        }
    );
}
