//! The executable's real dispatch functions on a synthetic account. Its normal build checks and concrete lab
//! startup remain in main; this module supplies only unit-test dependencies and cannot be selected at runtime.

#![cfg(test)]

mod calls;
mod printing;
mod sdp;
mod series;
mod support;
pub(super) use support::{Signals, TestLab};

use super::*;
use teams_fence::lab::Alias;
use teams_mock::{AB_CHAT, AX_CHAT, LAB_B};

fn command(args: &[&str]) -> Command {
    Cli::try_parse_from(["teams-headless", "a"].into_iter().chain(args.iter().copied()))
        .unwrap()
        .command
}

fn audio(args: &[&str]) -> CallAudio {
    match command(&[&["call", "echo"][..], args].concat()) {
        Command::Call { audio, .. } => audio,
        _ => unreachable!("a call command"),
    }
}

fn signals(stop_after: Option<Duration>) -> Signals {
    let mut signals = Signals::new().unwrap();
    signals.stop_after = stop_after;
    signals
}

async fn dispatch_args(lab: &TestLab, args: &[&str], stop_after: Option<Duration>) -> (Outcome, String) {
    let mut out = Vec::new();
    let outcome = tokio::time::timeout(
        Duration::from_secs(30),
        dispatch(lab, command(args), &mut signals(stop_after), &mut out),
    )
    .await
    .expect("an offline command finishes within its bound");
    (outcome, String::from_utf8(out).unwrap())
}

#[tokio::test]
async fn the_dispatcher_syncs_and_reads_the_store_without_opening_a_second_engine() {
    let lab = TestLab::new().await;
    let (synced, output) = dispatch_args(&lab, &["sync"], None).await;
    assert_eq!(synced.unwrap(), ExitCode::SUCCESS);
    assert!(output.contains("caught_up"), "{output}");
    let (listed, output) = dispatch_args(&lab, &["list", "--limit", "3"], None).await;
    assert_eq!(listed.unwrap(), ExitCode::SUCCESS);
    assert!(!output.is_empty());
    let (shown, output) = dispatch_args(&lab, &["show", "b", "--limit", "2"], None).await;
    assert_eq!(shown.unwrap(), ExitCode::SUCCESS);
    assert!(!output.is_empty());
    assert_eq!(conversation(&lab, "b").unwrap(), AB_CHAT);
    assert_eq!(lab.alias, Alias::A);
}

#[tokio::test]
async fn dispatched_writes_prove_a_denial_before_a_lab_send_and_apply_each_existing_action() {
    let lab = TestLab::new().await;
    let (denied, _) = dispatch_args(
        &lab,
        &["send", AX_CHAT, "must stay inside the fence", "--wait", "0"],
        None,
    )
    .await;
    assert_eq!(denied.unwrap(), ExitCode::FAILURE);
    let sent_before = lab
        .mock
        .received_requests()
        .await
        .into_iter()
        .filter(|request| {
            request.method.as_str() == "POST"
                && request
                    .url
                    .path()
                    .replace("%3A", ":")
                    .replace("%40", "@")
                    .contains(AX_CHAT)
        })
        .count();
    assert_eq!(sent_before, 0, "the outsider's chat received no post");
    for args in [
        vec!["send", "b", "synthetic new message"],
        vec!["edit", "b", "1760000010000", "synthetic edit"],
        vec!["react", "b", "1760000010000", "wow"],
        vec!["react", "b", "1760000010000", "wow", "--remove"],
        vec!["read", "b", "1760000010000"],
        vec!["delete", "b", "1760000010000"],
    ] {
        let (outcome, output) = dispatch_args(&lab, &args, None).await;
        assert_eq!(outcome.unwrap(), ExitCode::SUCCESS, "{args:?}: {output}");
    }
    assert!(write_action(&lab, command(&["sync"])).is_err());
    assert!(write_action(&lab, command(&["send", "not-a-chat", "text"])).is_err());
    let diagnostic = support::diagnostic_lines().join("\n");
    assert!(diagnostic.contains("action: send to "), "{diagnostic}");
    for private in ["synthetic new message", "synthetic edit", "must stay inside the fence"] {
        assert!(
            !diagnostic.contains(private),
            "the core's diagnostic log omits message text"
        );
    }
}

#[tokio::test]
async fn dispatched_presence_profile_and_search_print_their_results_or_failure() {
    let lab = TestLab::new().await;
    lab.mock.serve_presence(&[(LAB_B, "Busy")]).await;
    let (presence, output) = dispatch_args(&lab, &["presence", "--wait", "2", "--mri", LAB_B], None).await;
    assert_eq!(presence.unwrap(), ExitCode::SUCCESS, "{output}");
    assert!(output.contains("presence"));
    let (profile, output) = dispatch_args(&lab, &["profile", LAB_B, "--wait", "2"], None).await;
    assert_eq!(profile.unwrap(), ExitCode::SUCCESS, "{output}");
    assert!(output.contains("profile"));
    lab.mock
        .answer_next("POST", "suggestions", 200, &serde_json::json!({"Groups": []}))
        .await;
    lab.mock
        .answer_next("POST", "query", 200, &serde_json::json!({"EntitySets": []}))
        .await;
    let (found, output) = dispatch_args(&lab, &["search", "synthetic"], None).await;
    assert_eq!(found.unwrap(), ExitCode::SUCCESS);
    assert!(output.contains("search suggestions messages 0"));
    assert!(output.contains("search all people 0"));
    let (failed, output) = dispatch_args(&lab, &["search", "synthetic"], None).await;
    assert_eq!(failed.unwrap(), ExitCode::FAILURE);
    assert!(output.contains("failed"));
}

#[tokio::test]
async fn the_dispatcher_handles_startup_failures_and_signals_without_reaching_the_tenant() {
    let mut lab = TestLab::new().await;
    lab.fail_start = true;
    assert!(dispatch_args(&lab, &["sync"], None).await.0.is_err());
    lab.fail_start = false;
    for args in [
        vec!["sync"],
        vec!["send", "b", "interrupted"],
        vec!["presence"],
        vec!["profile", LAB_B],
    ] {
        let (outcome, _) = dispatch_args(&lab, &args, Some(Duration::ZERO)).await;
        assert_eq!(outcome.unwrap(), ExitCode::from(INTERRUPTED), "{args:?}");
    }
    let (watching, _) = dispatch_args(&lab, &["watch"], Some(Duration::ZERO)).await;
    assert_eq!(watching.unwrap(), ExitCode::SUCCESS);
    let (availability, _) = dispatch_args(&lab, &["availability", "--hold", "0"], Some(Duration::ZERO)).await;
    assert_eq!(availability.unwrap(), ExitCode::SUCCESS);
}

#[tokio::test]
async fn call_plans_video_flags_and_watch_outcomes_preserve_command_line_choices() {
    let plan = call_plan(7, 11, 13, Some(2));
    assert_eq!(
        (plan.hold, plan.connect_within, plan.invited_within),
        (Duration::from_secs(7), Duration::from_secs(11), Duration::from_secs(13))
    );
    assert_eq!(plan.camera_after, Some(Duration::from_secs(2)));
    assert!(call_plan(0, 0, 0, None).camera_after.is_none());
    assert!(video_of(&audio(&[])).0.is_none());
    let (video, counts) = video_of(&audio(&["--video"]));
    assert!(video.is_some() && counts.is_some());
    assert_eq!(watch_outcome(WatchEnd::Stopped).unwrap(), ExitCode::SUCCESS);
    assert_eq!(
        watch_outcome(WatchEnd::PushStopped(Stop::Requested)).unwrap(),
        ExitCode::SUCCESS
    );
    assert!(watch_outcome(WatchEnd::EngineStopped).is_err());
    assert!(
        calling_command(
            &TestLab::new().await,
            command(&["sync"]),
            &mut signals(None),
            &mut Vec::new()
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn call_files_use_private_lab_recordings_and_report_an_unopened_speaker() {
    let lab = TestLab::new().await;
    for burst in [false, true] {
        let audio = audio(if burst { &["--bursts"] } else { &[] });
        let mut output = Vec::new();
        let files = call_files(&lab, &audio, ("test", Duration::from_millis(200)), &mut output).unwrap();
        assert!(files.recording.starts_with(lab.paths().data_dir()));
        assert!(heard_lines(&files, &audio, false, &mut output).unwrap().is_none());
        assert!(String::from_utf8(output).unwrap().contains("echo heard=false"));
    }
    let mut audio = audio(&[]);
    audio.record = Some(lab.paths().data_dir().join("custom.wav"));
    let files = call_files(&lab, &audio, ("test", Duration::from_millis(200)), &mut Vec::new()).unwrap();
    assert_eq!(files.recording, audio.record.unwrap());
}
