//! Calls of ten people and more, as Teams lays them out: whatever the stage shows (the gallery, the speaker view, a
//! share with the people beside it) stays on the stage, under the call's controls, so the microphone and the rest take
//! their clicks however many are in the call and however small the window; the gallery shows equal tiles, as many to
//! a page as View's Max gallery size and the window allow, the account's own among them, the rest on pages turned
//! with `<` `>`; the strips page the same way. Every id and name is made up.

use gpui::{Bounds, Pixels, VisualTestContext, px, size};
use teams_core::{CallAction, Event};

use super::{actions, call, click, draw, drawn, group_call_up, view};

/// The window sizes looked at: the call window's smallest, a small one, a laptop's, a full HD screen's.
const SIZES: [(f32, f32); 4] = [(560.0, 400.0), (800.0, 560.0), (1280.0, 800.0), (1920.0, 1080.0)];

fn mris(count: usize) -> Vec<String> {
    (1..=count)
        .map(|n| format!("8:orgid:00000000-0000-4000-8000-000000000{n:03}"))
        .collect()
}

/// A group call of `count` others, the odd ones' cameras on.
fn large_call(rig: &super::Rig, count: usize) -> &'static mut VisualTestContext {
    let stage = group_call_up(rig);
    let ids = mris(count);
    let who: Vec<(&str, &str, bool, Option<usize>, bool)> = ids
        .iter()
        .enumerate()
        .map(|(index, mri)| (mri.as_str(), "Someone With A Long Name", index % 2 == 0, None, false))
        .collect();
    rig.send(Event::CallParticipants {
        call: call(1),
        view: view(&who, false),
    });
    rig.cx.run_until_parked();
    stage
}

fn bounds(stage: &mut VisualTestContext, selector: &str) -> Option<Bounds<Pixels>> {
    stage.debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
}

fn within(inner: Bounds<Pixels>, outer: Bounds<Pixels>) -> bool {
    inner.origin.x >= outer.origin.x - px(0.5)
        && inner.origin.y >= outer.origin.y - px(0.5)
        && inner.right() <= outer.right() + px(0.5)
        && inner.bottom() <= outer.bottom() + px(0.5)
}

/// Resizes the window and lets the stage take its new size.
fn resize(stage: &mut VisualTestContext, (width, height): (f32, f32)) {
    stage.simulate_resize(size(px(width), px(height)));
    draw(stage);
    draw(stage);
}

/// Every tile drawn sits on the stage, under the controls, and the microphone takes its click.
fn controls_clear(rig: &super::Rig, stage: &mut VisualTestContext, count: usize, case: &str) {
    draw(stage);
    let area = bounds(stage, "call-stage").expect("the stage");
    let mute = bounds(stage, "call-mute").expect("the microphone");
    assert!(
        mute.bottom() <= area.origin.y,
        "{case}: the microphone {mute:?} over the stage {area:?}"
    );
    let mut shown = 0;
    for index in 0..count {
        if let Some(tile) = bounds(stage, &format!("call-participant-{index}")) {
            shown += 1;
            assert!(
                within(tile, area),
                "{case}: tile {index} {tile:?} leaves the stage {area:?}"
            );
        }
    }
    assert!(shown > 0, "{case}: some tiles show");
    let before = actions(rig).len();
    click(stage, "call-mute");
    let asked = actions(rig);
    assert_eq!(asked.len(), before + 1, "{case}: the microphone took the click");
    assert!(
        matches!(asked.last(), Some(CallAction::Mute { .. })),
        "{case}: {asked:?}"
    );
}

#[test]
fn the_microphone_takes_its_click_in_a_gallery_of_10_to_50_at_every_window_size() {
    for count in [10, 16, 25, 50] {
        let rig = super::calling();
        let stage = large_call(&rig, count);
        for window in SIZES {
            resize(stage, window);
            controls_clear(&rig, stage, count, &format!("gallery of {count} at {window:?}"));
        }
    }
}

#[test]
fn the_microphone_takes_its_click_beside_a_share_and_in_the_speaker_view_of_a_large_call() {
    for count in [10, 25, 50] {
        let rig = super::calling();
        let stage = large_call(&rig, count);
        click(stage, "call-view");
        click(stage, "call-view-speaker");
        for window in SIZES {
            resize(stage, window);
            controls_clear(&rig, stage, count, &format!("speaker view of {count} at {window:?}"));
        }
        rig.send(Event::CallRemoteShare {
            call: call(1),
            on: true,
        });
        rig.cx.run_until_parked();
        assert!(drawn(stage, "call-share-strip"));
        for window in SIZES {
            resize(stage, window);
            controls_clear(&rig, stage, count, &format!("share with {count} at {window:?}"));
            let strip = bounds(stage, "call-share-strip").unwrap();
            for index in 0..count {
                if let Some(tile) = bounds(stage, &format!("call-participant-{index}")) {
                    assert!(
                        within(tile, strip),
                        "share of {count} at {window:?}: tile {index} leaves its strip"
                    );
                }
            }
        }
    }
}

/// The participants' tiles the stage shows, by index.
fn shown(stage: &mut VisualTestContext, count: usize) -> Vec<usize> {
    draw(stage);
    (0..count)
        .filter(|index| bounds(stage, &format!("call-participant-{index}")).is_some())
        .collect()
}

#[test]
fn a_gallery_of_more_than_fit_pages_with_the_account_s_own_tile_on_each_page() {
    let rig = super::calling();
    let stage = large_call(&rig, 60);
    resize(stage, (1920.0, 1080.0));
    // Teams' 49 to a page on a full HD screen, the account's own among them
    let first = shown(stage, 60);
    assert_eq!(first, (0..48).collect::<Vec<_>>());
    assert!(drawn(stage, "call-participant-self") && drawn(stage, "call-gallery-pager"));
    let tiles: Vec<Bounds<Pixels>> = first
        .iter()
        .map(|index| bounds(stage, &format!("call-participant-{index}")).unwrap())
        .collect();
    let own = bounds(stage, "call-participant-self").unwrap();
    for tile in &tiles {
        // equal 16:9 tiles
        assert!((tile.size.width - own.size.width).abs() < px(1.0));
        let ratio = f32::from(tile.size.width) / f32::from(tile.size.height);
        assert!((ratio - 16.0 / 9.0).abs() < 0.05, "{tile:?}");
    }
    // the first page has no way back; the next shows the rest and the account again
    click(stage, "call-gallery-previous");
    assert_eq!(shown(stage, 60), first);
    click(stage, "call-gallery-next");
    assert_eq!(shown(stage, 60), (48..60).collect::<Vec<_>>());
    assert!(drawn(stage, "call-participant-self"));
    click(stage, "call-gallery-next");
    assert_eq!(shown(stage, 60), (48..60).collect::<Vec<_>>(), "no page past the last");
    click(stage, "call-gallery-previous");
    assert_eq!(shown(stage, 60), first);

    // a small window shows fewer to a page, each tile still readable
    resize(stage, (800.0, 560.0));
    let small = shown(stage, 60);
    assert!(small.len() < 48 && small.len() >= 4, "{small:?}");
    let tile = bounds(stage, "call-participant-0").unwrap();
    assert!(tile.size.width >= px(150.0), "{tile:?}");
}

/// Picks `size` in View's Max gallery size (a submenu, reached by the keyboard as a user's arrows reach it).
fn pick_gallery_size(stage: &mut VisualTestContext, size: usize) {
    let item = format!("call-view-gallery-size-{size}");
    for downs in 1..=6 {
        click(stage, "call-view");
        stage.simulate_keystrokes(&format!("{} right", vec!["down"; downs].join(" ")));
        if drawn(stage, &item) {
            click(stage, &item);
            return;
        }
        stage.simulate_keystrokes("escape escape");
        draw(stage);
    }
    panic!("Max gallery size isn't in View");
}

#[test]
fn view_s_max_gallery_size_sets_how_many_a_page_shows() {
    let rig = super::calling();
    let stage = large_call(&rig, 16);
    resize(stage, (1920.0, 1080.0));
    assert_eq!(shown(stage, 16).len(), 16, "all 17 fit Teams' 49");
    assert!(!drawn(stage, "call-gallery-pager"));
    for (size, others) in [(4, 3), (9, 8), (16, 15), (49, 16)] {
        pick_gallery_size(stage, size);
        assert_eq!(shown(stage, 16).len(), others, "size {size}");
        assert_eq!(drawn(stage, "call-gallery-pager"), others < 16, "size {size}");
        assert!(drawn(stage, "call-participant-self"), "size {size}");
    }
}

#[test]
fn a_small_group_call_shows_everyone_on_one_page() {
    let rig = super::calling();
    let stage = large_call(&rig, 3);
    for window in SIZES {
        resize(stage, window);
        assert_eq!(shown(stage, 3), vec![0, 1, 2], "{window:?}");
        assert!(!drawn(stage, "call-gallery-pager"));
        assert!(drawn(stage, "call-participant-self"));
    }
}

#[test]
fn the_strips_page_with_the_account_s_own_tile_beside_the_share() {
    let rig = super::calling();
    let stage = large_call(&rig, 25);
    resize(stage, (1280.0, 800.0));
    // the gallery on its second page of 8 others and the account
    pick_gallery_size(stage, 9);
    click(stage, "call-gallery-next");
    let gallery = shown(stage, 25);
    assert_eq!(gallery, (8..16).collect::<Vec<_>>());
    rig.send(Event::CallRemoteShare {
        call: call(1),
        on: true,
    });
    rig.cx.run_until_parked();
    let first = shown(stage, 25);
    assert!(
        first.first() == Some(&0) && first.len() < 25,
        "the strip's first page: {first:?}"
    );
    assert!(drawn(stage, "call-strip-pager") && drawn(stage, "call-participant-self"));
    click(stage, "call-strip-next");
    let second = shown(stage, 25);
    assert_eq!(second.first(), Some(&first.len()), "the next ones");
    assert!(drawn(stage, "call-participant-self"));
    // the share ends: the gallery again, on the page it was on
    rig.send(Event::CallRemoteShare {
        call: call(1),
        on: false,
    });
    rig.cx.run_until_parked();
    assert!(!drawn(stage, "call-strip-pager"));
    assert_eq!(shown(stage, 25), gallery);
    // the speaker view's strip is another strip: its own first page, not the share strip's second
    click(stage, "call-view");
    click(stage, "call-view-speaker");
    let strip = shown(stage, 25);
    assert!(strip.contains(&0) && strip.contains(&1), "{strip:?}");
}

/// Whether `inner` and `outer` share any pixels.
fn overlap(inner: Bounds<Pixels>, outer: Bounds<Pixels>) -> bool {
    inner.right() > outer.origin.x + px(0.5)
        && outer.right() > inner.origin.x + px(0.5)
        && inner.bottom() > outer.origin.y + px(0.5)
        && outer.bottom() > inner.origin.y + px(0.5)
}

/// The strip's pager and tiles (all those shown but `large`) sit in `strip`, the account's own tile covers none of
/// them, and `>` turns the page.
fn strip_clear(stage: &mut VisualTestContext, (strip, large): (&str, Option<usize>), count: usize, case: &str) {
    draw(stage);
    let area = bounds(stage, strip).expect("the strip");
    let pager = bounds(stage, "call-strip-pager").expect("the strip pages");
    let own = bounds(stage, "call-participant-self").expect("the account's own tile");
    assert!(
        within(pager, area),
        "{case}: the pager {pager:?} leaves the strip {area:?}"
    );
    assert!(
        !overlap(pager, own),
        "{case}: the self view {own:?} covers the pager {pager:?}"
    );
    let first = shown(stage, count);
    for index in first.iter().filter(|index| Some(**index) != large) {
        let tile = bounds(stage, &format!("call-participant-{index}")).unwrap();
        assert!(
            within(tile, area),
            "{case}: tile {index} {tile:?} leaves the strip {area:?}"
        );
        assert!(!overlap(tile, own), "{case}: the self view covers tile {index}");
    }
    click(stage, "call-strip-next");
    assert_ne!(shown(stage, count), first, "{case}: > shows the next page");
    click(stage, "call-strip-previous");
}

#[test]
fn the_speaker_strip_s_pager_stays_clear_of_the_self_view_with_a_pane_open() {
    let rig = super::calling();
    let stage = large_call(&rig, 25);
    click(stage, "call-view");
    click(stage, "call-view-speaker");
    click(stage, "call-roster");
    assert!(drawn(stage, "call-roster-pane"));
    for window in [(560.0, 400.0), (700.0, 560.0), (800.0, 560.0), (1280.0, 800.0)] {
        resize(stage, window);
        strip_clear(
            stage,
            ("call-speaker-strip", Some(0)),
            25,
            &format!("speaker view with People at {window:?}"),
        );
    }
}

#[test]
fn the_share_strip_s_pager_stays_in_a_short_window_with_captions_and_the_timer_open() {
    let rig = super::calling();
    let stage = large_call(&rig, 25);
    rig.send(Event::CallRemoteShare {
        call: call(1),
        on: true,
    });
    super::caption_state(&rig, teams_core::CaptionPhase::On, None);
    rig.cx.run_until_parked();
    click(stage, "call-more");
    click(stage, "call-more-timer");
    for window in [(560.0, 400.0), (800.0, 440.0), (800.0, 512.0), (1280.0, 800.0)] {
        resize(stage, window);
        assert!(drawn(stage, "call-captions"), "captions show at {window:?}");
        assert!(drawn(stage, "call-timer"), "the timer shows at {window:?}");
        strip_clear(
            stage,
            ("call-share-strip", None),
            25,
            &format!("share with captions at {window:?}"),
        );
    }
}

#[test]
fn the_speaker_view_s_strip_pages() {
    let rig = super::calling();
    let stage = large_call(&rig, 25);
    resize(stage, (1280.0, 800.0));
    click(stage, "call-view");
    click(stage, "call-view-speaker");
    assert!(drawn(stage, "call-speaker-main") && drawn(stage, "call-strip-pager"));
    let first = shown(stage, 25);
    click(stage, "call-strip-next");
    let second = shown(stage, 25);
    assert_ne!(first, second);
    assert!(second.contains(&0), "the speaker stays large: {second:?}");
}

#[test]
fn the_captions_and_the_stage_stay_in_the_smallest_window_while_reconnecting() {
    let rig = super::calling();
    let stage = large_call(&rig, 25);
    super::caption_state(&rig, teams_core::CaptionPhase::On, None);
    super::status(&rig, 1, teams_core::CallStatus::Reconnecting);
    rig.cx.run_until_parked();
    for top in [false, true] {
        if top {
            // the menu's submenus open to the left in a wider window
            resize(stage, (1280.0, 800.0));
            click(stage, "call-caption-settings");
            stage.simulate_keystrokes("down down down down left down enter");
        }
        resize(stage, (560.0, 400.0));
        assert!(drawn(stage, "call-reconnecting"));
        let window = bounds(stage, "call-window").unwrap();
        let captions = bounds(stage, "call-captions").expect("the captions");
        let area = bounds(stage, "call-stage").expect("the stage");
        let pager = bounds(stage, "call-gallery-pager").expect("the gallery pages");
        assert!((captions.origin.y < area.origin.y) == top, "captions on top: {top}");
        for (what, inner) in [("captions", captions), ("stage", area), ("pager", pager)] {
            assert!(
                within(inner, window),
                "captions on top {top}: the {what} {inner:?} leaves the window {window:?}"
            );
        }
    }
}

#[test]
fn the_caption_language_confirmation_s_actions_stay_in_a_short_window() {
    let rig = super::calling();
    let stage = large_call(&rig, 25);
    super::caption_state(&rig, teams_core::CaptionPhase::On, None);
    rig.cx.run_until_parked();
    resize(stage, (560.0, 448.0));
    super::caption_language_menu(stage);
    click(stage, "call-caption-language-sq-al");
    draw(stage);
    let window = bounds(stage, "call-window").unwrap();
    let confirm = bounds(stage, "call-caption-language-confirm").expect("the confirmation");
    let captions = bounds(stage, "call-captions").unwrap();
    assert!(
        within(confirm, captions) && within(captions, window),
        "the confirmation {confirm:?} in the captions {captions:?} in the window {window:?}"
    );
    super::click_caption_confirmation(stage, true);
    assert!(
        matches!(actions(&rig).last(), Some(CallAction::CaptionLanguage { language, .. }) if language == "sq-al"),
        "Update took the click"
    );
}

/// Opens View with the keyboard on Max gallery size, its submenu shown beside it.
fn view_on_gallery_sizes(stage: &mut VisualTestContext) {
    for downs in 1..=6 {
        click(stage, "call-view");
        stage.simulate_keystrokes(&vec!["down"; downs].join(" "));
        draw(stage);
        if drawn(stage, "call-view-gallery-size-4") {
            return;
        }
        stage.simulate_keystrokes("escape");
        draw(stage);
    }
    panic!("Max gallery size isn't in View");
}

#[test]
fn view_s_max_gallery_size_is_picked_by_the_keyboard() {
    let rig = super::calling();
    let stage = large_call(&rig, 16);
    for window in [(1920.0, 1080.0), (800.0, 560.0), (560.0, 400.0)] {
        resize(stage, window);
        // the arrow toward the sizes goes in (Right where they open on the right, as Teams' do), Down moves on,
        // Enter picks
        view_on_gallery_sizes(stage);
        let parent = bounds(stage, "call-view-videos-first").expect("View's rows");
        let sizes = bounds(stage, "call-view-gallery-size-4").expect("the sizes");
        let (into, back) = if sizes.origin.x > parent.right() {
            ("right", "left")
        } else {
            assert!(
                window.0 < 800.0,
                "{window:?}: the sizes open on the right where there is room"
            );
            ("left", "right")
        };
        stage.simulate_keystrokes(&format!("{into} down enter"));
        draw(stage);
        assert!(!drawn(stage, "call-view-gallery"), "{window:?}: picking closes View");
        if window.0 >= 800.0 {
            assert_eq!(shown(stage, 16).len(), 8, "{window:?}: 9 picked");
        }
        // Escape and the arrow back go back to View's rows, still open, and the arrow in goes in again
        view_on_gallery_sizes(stage);
        stage.simulate_keystrokes(&format!("{into} escape"));
        draw(stage);
        assert!(drawn(stage, "call-view-gallery"), "{window:?}: Escape leaves View open");
        stage.simulate_keystrokes(&format!("{into} {back} {into} enter"));
        draw(stage);
        assert!(
            !drawn(stage, "call-view-gallery"),
            "{window:?}: picked after going back"
        );
        assert_eq!(shown(stage, 16).len(), 3, "{window:?}: 4 picked after going back");
        pick_gallery_size(stage, 49);
    }
}

/// The roster of [`large_call`] again, `sharer` sharing its screen and `speaking` speaking.
fn tell(rig: &super::Rig, count: usize, sharer: Option<usize>, speaking: &[usize]) {
    let ids = mris(count);
    let who: Vec<(&str, &str, bool, Option<usize>, bool)> = ids
        .iter()
        .enumerate()
        .map(|(index, mri)| {
            let speaks = speaking.contains(&index);
            (mri.as_str(), "Someone With A Long Name", index % 2 == 0, None, speaks)
        })
        .collect();
    let mut told = view(&who, false);
    if let Some(sharer) = sharer {
        told.participants[sharer].sharing = true;
    }
    rig.send(Event::CallParticipants {
        call: call(1),
        view: told,
    });
    rig.cx.run_until_parked();
}

/// The roster indices of the tiles drawn, in the order they show: top to bottom, left to right.
fn in_order(stage: &mut VisualTestContext, count: usize) -> Vec<usize> {
    let mut drawn: Vec<(usize, Bounds<Pixels>)> = shown(stage, count)
        .into_iter()
        .map(|index| (index, bounds(stage, &format!("call-participant-{index}")).unwrap()))
        .collect();
    drawn.sort_by(|(_, a), (_, b)| {
        (f32::from(a.origin.y), f32::from(a.origin.x))
            .partial_cmp(&(f32::from(b.origin.y), f32::from(b.origin.x)))
            .unwrap()
    });
    drawn.into_iter().map(|(index, _)| index).collect()
}

/// Moves the stage's clock on by `ms` and lets what was due run.
fn later(rig: &super::Rig, stage: &mut VisualTestContext, ms: u64) {
    stage.executor().advance_clock(std::time::Duration::from_millis(ms));
    rig.cx.run_until_parked();
    draw(stage);
}

/// A share of 25 others at a laptop's size, `sharer` sharing.
fn shared_by(rig: &super::Rig, sharer: usize) -> &'static mut VisualTestContext {
    let stage = large_call(rig, 25);
    resize(stage, (1280.0, 800.0));
    tell(rig, 25, Some(sharer), &[]);
    rig.send(Event::CallRemoteShare {
        call: call(1),
        on: true,
    });
    rig.cx.run_until_parked();
    draw(stage);
    stage
}

#[test]
fn the_presenter_leads_the_share_strip_and_the_account_s_own_tile_ends_each_page() {
    let rig = super::calling();
    let stage = shared_by(&rig, 20);
    let first = in_order(stage, 25);
    assert_eq!(first.first(), Some(&20), "the presenter first: {first:?}");
    assert_eq!(
        &first[1..],
        &(0..first.len() - 1).collect::<Vec<_>>()[..],
        "then the roster's order"
    );
    let last_tile = bounds(stage, &format!("call-participant-{}", first[first.len() - 1])).unwrap();
    let own = bounds(stage, "call-participant-self").expect("the account's own tile");
    assert!(own.origin.y > last_tile.origin.y, "the account's own tile last");
    click(stage, "call-strip-next");
    assert!(
        !in_order(stage, 25).contains(&20),
        "the presenter only on the first page"
    );
    assert!(
        drawn(stage, "call-participant-self"),
        "the account's own tile on the next page too"
    );
}

#[test]
fn a_speaker_on_page_2_comes_to_page_1_but_a_short_sound_moves_nothing() {
    let rig = super::calling();
    let stage = shared_by(&rig, 20);
    let first = in_order(stage, 25);
    assert!(!first.contains(&18), "18 starts on a later page: {first:?}");
    // a cough: speaking for half a second moves nobody
    tell(&rig, 25, Some(20), &[18]);
    later(&rig, stage, 500);
    tell(&rig, 25, Some(20), &[]);
    later(&rig, stage, 5_000);
    assert_eq!(in_order(stage, 25), first, "no jump on a short sound");
    // 18 talks: once it spoke a second it moves up, right after the presenter
    tell(&rig, 25, Some(20), &[18]);
    assert_eq!(in_order(stage, 25), first, "not at once");
    later(&rig, stage, 1_000);
    let moved = in_order(stage, 25);
    assert_eq!(&moved[..2], &[20, 18], "the speaker next to the presenter: {moved:?}");
    assert_eq!(moved.len(), first.len(), "the page holds as many");
    // 22 answers at once: the order holds still for a few seconds, then 22 leads the speakers
    tell(&rig, 25, Some(20), &[22]);
    later(&rig, stage, 1_000);
    assert_eq!(in_order(stage, 25), moved, "no second jump right after the first");
    later(&rig, stage, 2_000);
    let then = in_order(stage, 25);
    assert_eq!(&then[..3], &[20, 22, 18], "{then:?}");
    // 18 talks again, in sight: it stays where it is
    tell(&rig, 25, Some(20), &[18]);
    later(&rig, stage, 10_000);
    assert_eq!(in_order(stage, 25), then);
}

#[test]
fn the_page_the_user_turned_to_stays_while_someone_on_it_speaks() {
    let rig = super::calling();
    let stage = shared_by(&rig, 0);
    click(stage, "call-strip-next");
    let second = in_order(stage, 25);
    assert!(drawn(stage, "call-strip-pager"));
    let speaker = second[1];
    tell(&rig, 25, Some(0), &[speaker]);
    later(&rig, stage, 5_000);
    assert_eq!(
        in_order(stage, 25),
        second,
        "a tile in sight keeps its place, the page stays"
    );
    // it stopped: back on the first page, it did not move while in sight
    tell(&rig, 25, Some(0), &[]);
    click(stage, "call-strip-previous");
    assert!(!in_order(stage, 25).contains(&speaker));
    // one who still speaks as the page turns away from it comes up
    click(stage, "call-strip-next");
    tell(&rig, 25, Some(0), &[speaker]);
    later(&rig, stage, 2_000);
    click(stage, "call-strip-previous");
    later(&rig, stage, 0);
    let first = in_order(stage, 25);
    assert_eq!(&first[..2], &[0, speaker], "{first:?}");
}

#[test]
fn a_gallery_speaker_on_a_later_page_comes_to_the_first() {
    let rig = super::calling();
    let stage = large_call(&rig, 25);
    resize(stage, (1280.0, 800.0));
    pick_gallery_size(stage, 9);
    let first = in_order(stage, 25);
    assert_eq!(first, (0..8).collect::<Vec<_>>());
    tell(&rig, 25, None, &[17]);
    later(&rig, stage, 1_000);
    let moved = in_order(stage, 25);
    assert_eq!(moved.first(), Some(&17), "{moved:?}");
    assert_eq!(moved.len(), 8);
}

#[test]
fn a_speaker_who_goes_out_of_sight_without_a_calls_event_still_comes_to_the_first_page() {
    let rig = super::calling();
    let stage = large_call(&rig, 25);
    resize(stage, (1280.0, 800.0));
    pick_gallery_size(stage, 16);
    assert!(in_order(stage, 25).contains(&12), "12 starts in sight");
    // 12 speaks in sight: it keeps its place
    tell(&rig, 25, None, &[12]);
    later(&rig, stage, 2_000);
    assert!(in_order(stage, 25).contains(&12));
    // a smaller gallery leaves 12 out with no calls event: it still comes up, it spoke long enough
    pick_gallery_size(stage, 9);
    later(&rig, stage, 0);
    let moved = in_order(stage, 25);
    assert_eq!(moved.first(), Some(&12), "{moved:?}");
}
