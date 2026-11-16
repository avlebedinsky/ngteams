//! A group call's video through the server's own media node (`ngteams_server::mixer`): the client's engines turn
//! their cameras on, the roster tells it, each client asks the node for the others' video on its receiving lines
//! (source requests), and the node forwards each camera onto the lines that asked for it; a shared screen comes on
//! the others' sharing line. Each camera's picture has a shape of its own, so a line shows whose video it carries.

use std::time::Duration;

use teams_core::{CallAction, CallId, CallParticipant, CallStatus, CallView};
use teams_media::devices::file::PatternCamera;
use teams_media::session::SHARING_LINE;

use super::engines::{A_TONE, B_TONE, Engine, FRAMES, wait_until};
use super::group_media::{C_TONE, Mixed, connect_all, group_chat, hang_up_all};
use crate::support::backends;

/// The cameras' shapes: wide, four by three, square.
const WIDE: PatternCamera = PatternCamera {
    width: 640,
    height: 360,
    fps: 15,
};
const FOUR_THREE: PatternCamera = PatternCamera {
    width: 480,
    height: 360,
    fps: 15,
};
const SQUARE: PatternCamera = PatternCamera {
    width: 360,
    height: 360,
    fps: 15,
};

/// A picture's shape: its width over its height.
fn shape(width: u32, height: u32) -> f32 {
    #[expect(clippy::cast_precision_loss, reason = "a picture's sides are a few thousand at most")]
    let (width, height) = (width as f32, height as f32);
    width / height.max(1.0)
}

/// The participant `mri` as `view` shows it.
fn party<'a>(view: &'a CallView, mri: &str) -> Option<&'a CallParticipant> {
    view.participants.iter().find(|party| party.mri == mri)
}

/// Waits until `engine` shows each of `others` with its camera on, on a receiving line of its own, and that line has
/// shown at least [`FRAMES`] frames of the shape the other's camera has. Gives the line of each other, in order.
async fn sees(engine: &mut Engine, call: CallId, others: &[(&str, PatternCamera)]) -> Vec<usize> {
    let view = engine
        .view(call, |view| {
            others.iter().all(|(mri, _)| {
                party(view, mri).is_some_and(|party| party.video_on && party.line.is_some_and(|line| line >= 1))
            })
        })
        .await;
    let mut lines = Vec::new();
    for (mri, camera) in others {
        let line = party(&view, mri).and_then(|party| party.line).unwrap();
        lines.push(line);
        let wanted = shape(camera.width, camera.height);
        let what = format!("{}: {mri}'s video on line {line}", engine.mri);
        wait_until(&what, || {
            let seen = engine.seen(line);
            seen.frames >= FRAMES && (shape(seen.width, seen.height) - wanted).abs() < 0.1
        })
        .await;
        eprintln!("{what}: {:?}", engine.seen(line));
    }
    lines
}

/// Turns `engine`'s camera in `call` on or off.
async fn camera(engine: &Engine, call: CallId, on: bool) {
    engine.call(CallAction::Camera { call, on }).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_clients_see_each_others_video_on_the_lines_they_asked_for() {
    for backend in backends() {
        let mixed = Mixed::start(backend).await;
        let mut a = mixed.engine_filmed("Ada", A_TONE, WIDE).await;
        let mut b = mixed.engine_filmed("Bert", B_TONE, FOUR_THREE).await;
        let mut c = mixed.engine_filmed("Cora", C_TONE, SQUARE).await;
        let chat = group_chat(&a, &[&b, &c], &[]).await;
        let calls = connect_all(&chat, &mut [&mut a, &mut b, &mut c]).await;
        mixed.connected(&mixed.open_call().await, 3).await;
        for (engine, call) in [(&a, calls[0]), (&b, calls[1]), (&c, calls[2])] {
            camera(engine, call, true).await;
        }
        let (a_mri, b_mri, c_mri) = (a.mri.clone(), b.mri.clone(), c.mri.clone());
        sees(&mut a, calls[0], &[(&b_mri, FOUR_THREE), (&c_mri, SQUARE)]).await;
        let lines = [
            sees(&mut b, calls[1], &[(&a_mri, WIDE), (&c_mri, SQUARE)]).await[0],
            sees(&mut c, calls[2], &[(&a_mri, WIDE), (&b_mri, FOUR_THREE)]).await[0],
        ];
        // a's camera off: the others are told, and its line shows nothing more
        camera(&a, calls[0], false).await;
        for (engine, call) in [(&mut b, calls[1]), (&mut c, calls[2])] {
            engine
                .view(call, |view| party(view, &a_mri).is_some_and(|party| !party.video_on))
                .await;
        }
        let shown = [b.seen(lines[0]).frames, c.seen(lines[1]).frames];
        tokio::time::sleep(Duration::from_secs(1)).await;
        for (engine, line, shown) in [(&b, lines[0], shown[0]), (&c, lines[1], shown[1])] {
            assert!(
                engine.seen(line).frames <= shown + 2,
                "{}: a's camera still came after it was turned off",
                engine.mri
            );
        }
        hang_up_all(&mut [&mut a, &mut b, &mut c], &calls).await;
        for engine in [a, b, c] {
            engine.shut_down().await;
        }
        mixed.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_late_joiner_sees_the_cameras_that_were_on_and_is_seen() {
    for backend in backends() {
        let mixed = Mixed::start(backend).await;
        let mut a = mixed.engine_filmed("Ada", A_TONE, WIDE).await;
        let mut b = mixed.engine_filmed("Bert", B_TONE, FOUR_THREE).await;
        let mut c = mixed.engine_filmed("Cora", C_TONE, SQUARE).await;
        let chat = group_chat(&a, &[&b, &c], &[]).await;
        let mut calls = connect_all(&chat, &mut [&mut a, &mut b]).await;
        let rung = c.incoming().await;
        c.call(CallAction::Decline { call: rung.call }).await;
        camera(&a, calls[0], true).await;
        camera(&b, calls[1], true).await;
        let (a_mri, b_mri, c_mri) = (a.mri.clone(), b.mri.clone(), c.mri.clone());
        sees(&mut a, calls[0], &[(&b_mri, FOUR_THREE)]).await;
        // c joins the call it finds in the chat: the running cameras come to it from their next keyframe
        assert!(c.handle.sync().await.unwrap().failed.is_empty());
        let joined = c.call(CallAction::Join { chat: chat.clone() }).await;
        c.status(joined, &CallStatus::Connected).await;
        calls.push(joined);
        sees(&mut c, joined, &[(&a_mri, WIDE), (&b_mri, FOUR_THREE)]).await;
        camera(&c, joined, true).await;
        sees(&mut a, calls[0], &[(&c_mri, SQUARE)]).await;
        hang_up_all(&mut [&mut a, &mut b, &mut c], &calls).await;
        for engine in [a, b, c] {
            engine.shut_down().await;
        }
        mixed.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_shared_screen_reaches_the_others_until_it_stops() {
    for backend in backends() {
        let mixed = Mixed::start(backend).await;
        let mut a = mixed.engine("Ada", A_TONE).await;
        let mut b = mixed.engine("Bert", B_TONE).await;
        let mut c = mixed.engine("Cora", C_TONE).await;
        let chat = group_chat(&a, &[&b, &c], &[]).await;
        let calls = connect_all(&chat, &mut [&mut a, &mut b, &mut c]).await;
        mixed.connected(&mixed.open_call().await, 3).await;
        b.call(CallAction::Share {
            call: calls[1],
            on: true,
        })
        .await;
        let sharer = b.mri.clone();
        for (engine, call) in [(&mut a, calls[0]), (&mut c, calls[2])] {
            engine
                .view(call, |view| party(view, &sharer).is_some_and(|party| party.sharing))
                .await;
            let what = format!("{}: b's screen", engine.mri);
            wait_until(&what, || {
                let seen = engine.seen(SHARING_LINE);
                seen.frames >= FRAMES && (shape(seen.width, seen.height) - 16.0 / 9.0).abs() < 0.1
            })
            .await;
            eprintln!("{what}: {:?}", engine.seen(SHARING_LINE));
        }
        b.call(CallAction::Share {
            call: calls[1],
            on: false,
        })
        .await;
        for (engine, call) in [(&mut a, calls[0]), (&mut c, calls[2])] {
            engine
                .view(call, |view| party(view, &sharer).is_some_and(|party| !party.sharing))
                .await;
        }
        let shown = a.seen(SHARING_LINE).frames;
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(
            a.seen(SHARING_LINE).frames <= shown + 2,
            "b's screen still came after its stop"
        );
        hang_up_all(&mut [&mut a, &mut b, &mut c], &calls).await;
        for engine in [a, b, c] {
            engine.shut_down().await;
        }
        mixed.stop().await;
    }
}
