//! Which synthetic captions go to the tool, when, and with what context.

use super::*;

fn line(stamp: u64, text: &str, final_result: bool) -> Caption {
    Caption {
        id: format!("synthetic/{stamp}"),
        speaker: Some("8:orgid:synthetic-a".into()),
        display_name: Some("Lab Speaker".into()),
        text: text.into(),
        final_result,
        spoken_language: "en-us".into(),
        track_language: "en-us".into(),
        audio_timestamp: stamp,
        duration: 10,
    }
}

fn sent(next: Next) -> Job {
    match next {
        Next::Send(job) => job,
        other => panic!("nothing sent: {other:?}"),
    }
}

#[test]
fn finals_go_one_at_a_time_in_order_and_show_their_translation() {
    let now = Instant::now();
    let mut schedule = Schedule::new("de");
    let lines = [line(1, "One.", true), line(2, "Two.", true)];
    schedule.observe(&lines, now);
    let first = sent(schedule.next(now));
    assert_eq!(first.source, "One.");
    assert!(first.final_result);
    assert_eq!(first.speaker.as_deref(), Some("Lab Speaker"));
    assert_eq!(schedule.next(now), Next::Idle, "one at a time");
    schedule.done(first, "Eins.".into());
    assert_eq!(schedule.text(&lines[0]), Some("Eins."));
    assert_eq!(schedule.text(&lines[1]), None);
    // seen again, nothing goes twice
    schedule.observe(&lines, now);
    let second = sent(schedule.next(now));
    assert_eq!(second.source, "Two.");
    schedule.done(second, "Zwei.".into());
    schedule.observe(&lines, now);
    assert_eq!(schedule.next(now), Next::Idle);
}

#[test]
fn interim_text_waits_to_settle_or_refreshes_while_it_keeps_changing() {
    let start = Instant::now();
    let mut schedule = Schedule::new("de");
    schedule.observe(&[line(1, "Good", false)], start);
    assert_eq!(schedule.next(start), Next::At(start + SETTLE));
    let later = start + Duration::from_millis(300);
    schedule.observe(&[line(1, "Good morning", false)], later);
    assert_eq!(schedule.next(later), Next::At(later + SETTLE));
    // still changing: it goes at the refresh anyway, its latest text
    let changing = start + Duration::from_millis(1000);
    schedule.observe(&[line(1, "Good morning every", false)], changing);
    assert_eq!(schedule.next(changing), Next::At(start + REFRESH));
    let job = sent(schedule.next(start + REFRESH));
    assert_eq!(job.source, "Good morning every");
    assert!(!job.final_result);
    schedule.done(job, "Guten Morgen all".into());
    assert_eq!(
        schedule.text(&line(1, "Good morning everyone", false)),
        Some("Guten Morgen all")
    );
}

#[test]
fn a_final_overtakes_its_interim_and_a_late_interim_answer_never_replaces_it() {
    let now = Instant::now();
    let mut schedule = Schedule::new("de");
    schedule.observe(&[line(1, "Good", false)], now);
    let interim = sent(schedule.next(now + SETTLE));
    schedule.observe(&[line(1, "Good morning.", true)], now);
    // the final waits for the interim's answer
    assert_eq!(schedule.next(now), Next::Idle);
    schedule.done(interim.clone(), "Gut".into());
    let final_job = sent(schedule.next(now));
    assert_eq!(final_job.source, "Good morning.");
    schedule.done(final_job, "Guten Morgen.".into());
    schedule.busy = true;
    schedule.done(interim, "Gut".into());
    assert_eq!(schedule.text(&line(1, "Good morning.", true)), Some("Guten Morgen."));
}

#[test]
fn the_cache_answers_a_repeated_text_and_it_goes_as_context() {
    let now = Instant::now();
    let mut schedule = Schedule::new("de");
    schedule.observe(&[line(1, "Thanks.", false)], now);
    let job = sent(schedule.next(now + SETTLE));
    schedule.done(job, "Danke.".into());
    // the final repeating the interim text costs nothing
    let lines = [line(1, "Thanks.", true), line(2, "Next.", true)];
    schedule.observe(&lines, now);
    assert_eq!(schedule.text(&lines[0]), Some("Danke."));
    let next = sent(schedule.next(now));
    assert_eq!(next.source, "Next.");
    assert_eq!(
        next.said,
        vec![Said {
            speaker: Some("Lab Speaker".into()),
            text: "Thanks.".into()
        }]
    );
}

#[test]
fn speech_already_in_the_target_language_shows_as_spoken_and_goes_as_context() {
    let now = Instant::now();
    let mut schedule = Schedule::new("en");
    let mut german = line(2, "Hallo.", true);
    german.track_language = "de-de".into();
    let lines = [line(1, "Hello.", true), german];
    schedule.observe(&lines, now);
    assert!(schedule.in_target(&lines[0]));
    assert!(!schedule.in_target(&lines[1]));
    let job = sent(schedule.next(now));
    assert_eq!(job.source, "Hallo.");
    assert_eq!(job.said.len(), 1);
    assert_eq!(job.said[0].text, "Hello.");
}

#[test]
fn a_full_queue_sends_its_oldest_as_context_only() {
    let now = Instant::now();
    let mut schedule = Schedule::new("de");
    let lines: Vec<Caption> = (1..=(MAX_QUEUE as u64 + 2))
        .map(|stamp| line(stamp, &format!("Line {stamp}."), true))
        .collect();
    schedule.observe(&lines, now);
    let job = sent(schedule.next(now));
    assert_eq!(job.source, "Line 3.");
    let said: Vec<&str> = job.said.iter().map(|said| said.text.as_str()).collect();
    assert_eq!(said, ["Line 1.", "Line 2."]);
}

#[test]
fn the_brief_goes_when_it_changes() {
    let now = Instant::now();
    let mut schedule = Schedule::new("de");
    schedule.set_brief("Title: Synthetic".into());
    schedule.observe(&[line(1, "One.", true), line(2, "Two.", true)], now);
    let first = sent(schedule.next(now));
    assert_eq!(first.brief.as_deref(), Some("Title: Synthetic"));
    schedule.done(first, "Eins.".into());
    let second = sent(schedule.next(now));
    assert_eq!(second.brief, None);
    schedule.done(second, "Zwei.".into());
    schedule.set_brief("Title: Synthetic\nIn the call: Lab Speaker".into());
    schedule.observe(&[line(3, "Three.", true)], now);
    assert!(sent(schedule.next(now)).brief.is_some());
}

#[test]
fn translations_go_with_their_captions() {
    let now = Instant::now();
    let mut schedule = Schedule::new("de");
    let kept = line(1, "One.", true);
    schedule.observe(std::slice::from_ref(&kept), now);
    let job = sent(schedule.next(now));
    schedule.done(job, "Eins.".into());
    schedule.observe(&[], now);
    assert_eq!(schedule.text(&kept), None);
    assert!(schedule.noted.is_empty());
    // a caption without an audio timestamp is told by its id
    let mut unstamped = line(0, "X.", true);
    unstamped.id = "synthetic/a".into();
    let mut other = unstamped.clone();
    other.id = "synthetic/b".into();
    assert_ne!(LineKey::of(&unstamped), LineKey::of(&other));
}

#[test]
fn the_cache_is_bounded() {
    let now = Instant::now();
    let mut schedule = Schedule::new("de");
    for stamp in 0..(CACHE as u64 + 10) {
        schedule.observe(&[line(stamp, &format!("Line {stamp}."), true)], now);
        let job = sent(schedule.next(now));
        schedule.done(job, "x".into());
    }
    assert_eq!(schedule.cache.len(), CACHE);
    assert!(!schedule.cache.contains_key("Line 0."));
}
