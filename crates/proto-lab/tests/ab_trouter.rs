//! The `ab-trouter` scenario's offline parts (M1 Task 14, `docs/protocol/trouter.md`): its names, the frame classifier
//! of each variant, and how it judges its denies. All URLs and ids here are made up.

use proto_lab::classify::{Classification, OwnKind};
use proto_lab::fence_http::{Denial, DenyCause};
use proto_lab::fence_ws::{Frame, FrameClassifier, FrameDenial, Payload};
use proto_lab::scenario::ab_trouter::{self, Activity, TROUTER_RUN_REQUESTS};
use proto_lab::scenario::{ScenarioName, ab_chat};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// The a–b chat of two made-up lab accounts.
const AB_CHAT: &str = "19:0000000a-0000-4000-8000-00000000000a_0000000b-0000-4000-8000-00000000000b@unq.gbl.spaces";

const REGIONAL: &str = "wss://pub-ent-zz-01-t.trouter.teams.microsoft.com/v4/c?tc=%7B%7D";

const ACTIVITY: &str = r#"5:4+::{"name":"user.activity","args":[{"state":"active","cv":"SyntheticCorrelation0.0.1"}]}"#;

fn denial(method: &str, url: &str) -> Denial {
    Denial::new(method, url, DenyCause::Unclassified)
}

#[test]
fn the_two_variants_have_their_own_names() {
    assert_eq!(ScenarioName::AbTrouter.as_str(), "ab-trouter");
    assert_eq!(ScenarioName::AbTrouterQuiet.to_string(), "ab-trouter-quiet");
    assert_eq!(ab_trouter::scenario(Activity::Reported), ScenarioName::AbTrouter);
    assert_eq!(ab_trouter::scenario(Activity::HeldBack), ScenarioName::AbTrouterQuiet);
}

#[test]
fn only_the_quiet_variant_holds_back_the_activity() {
    let frame = Frame {
        url: REGIONAL.to_owned(),
        payload: Payload::Text(ACTIVITY.to_owned()),
    };
    assert_eq!(
        ab_trouter::frames(Activity::Reported).classify(&frame),
        Classification::Own(OwnKind::TrouterSession)
    );
    assert_eq!(
        ab_trouter::frames(Activity::HeldBack).classify(&frame),
        Classification::Unknown
    );
    assert_eq!(ab_trouter::frames(Activity::Reported), FrameClassifier::messaging());
}

#[test]
fn the_run_expects_what_only_the_push_channel_brings_up() -> TestResult {
    let policy = ab_trouter::trouter_policy(Some(AB_CHAT))?;
    for (method, url) in [
        ("POST", "https://teams.microsoft.com/registrar/prod/V2/registrations"),
        ("POST", "https://teams.cloud.microsoft/registrar/prod/V2/registrations"),
        ("PUT", "https://teams.microsoft.com/ups/noam/v1/me/endpoints/"),
        ("POST", "https://teams.microsoft.com/ups/noam/v1/pubsub/subscriptions/x"),
        (
            "PUT",
            "https://teams.cloud.microsoft/api/chatsvc/amer/v1/users/ME/conversations/48%3Anotifications/messages/1700000000000/properties?name=isread",
        ),
        (
            "POST",
            "https://teams.cloud.microsoft/api/chatsvc/amer/v1/agents/28:00000000-0000-4000-8000-0000000000aa/invoke",
        ),
        // the list's classes still hold
        ("POST", "https://teams.microsoft.com/ups/noam/v1/presence/getpresence/"),
    ] {
        assert!(policy.expects(&denial(method, url)), "{method} {url}");
    }
    // and `ab-chat` keeps them unexpected
    let ab_chat = ab_chat::policy(Some(AB_CHAT))?;
    for entry in TROUTER_RUN_REQUESTS {
        let url = format!("https://teams.microsoft.com{}", entry.path.replace('*', "x"));
        assert!(!ab_chat.expects(&denial(entry.method, &url)), "{}", entry.class);
    }
    Ok(())
}

#[test]
fn the_run_s_own_classes_hold_only_as_the_list_s_do() -> TestResult {
    let policy = ab_trouter::trouter_policy(Some(AB_CHAT))?;
    for (method, url, cause) in [
        // another method, path or host
        (
            "PUT",
            "https://teams.microsoft.com/registrar/prod/V2/registrations",
            DenyCause::Unclassified,
        ),
        (
            "POST",
            "https://teams.microsoft.com/registrar/prod/V2/registrations/x",
            DenyCause::Unclassified,
        ),
        (
            "POST",
            "https://example.com/registrar/prod/V2/registrations",
            DenyCause::Unclassified,
        ),
        (
            "DELETE",
            "https://teams.microsoft.com/ups/noam/v1/me/endpoints/",
            DenyCause::Unclassified,
        ),
        // the feed's read state on a real chat, or an agent's other operations
        (
            "PUT",
            "https://teams.microsoft.com/api/chatsvc/amer/v1/users/ME/conversations/19%3Ax%40thread.v2/messages/1/properties",
            DenyCause::Unclassified,
        ),
        (
            "POST",
            "https://teams.microsoft.com/api/chatsvc/amer/v1/agents/28:x/messages",
            DenyCause::Unclassified,
        ),
        // a deny for another cause than an unknown request
        (
            "POST",
            "https://teams.microsoft.com/registrar/prod/V2/registrations",
            DenyCause::Malformed,
        ),
    ] {
        assert!(
            !policy.expects(&Denial::new(method, url, cause)),
            "{method} {url} {cause:?}"
        );
    }
    // a write on the a–b chat stays unexpected
    let chat = format!(
        "https://teams.microsoft.com/api/chatsvc/amer/v1/users/ME/conversations/{}/messages",
        AB_CHAT.replace(':', "%3A").replace('@', "%40")
    );
    assert!(!policy.expects(&denial("POST", &chat)));
    Ok(())
}

#[test]
fn the_edit_adds_no_space_and_the_reported_run_outlasts_a_delivery_s_lifetime() {
    let edited = ab_trouter::edited_text("n0");
    assert!(edited.starts_with(&ab_chat::message_text("n0")));
    assert_eq!(
        edited.matches(' ').count(),
        ab_chat::message_text("n0").matches(' ').count()
    );
    // a chat delivery carries `ttl=900` (seconds)
    assert!(ab_trouter::observation(Activity::Reported).as_secs() > 900);
    assert!(ab_trouter::observation(Activity::HeldBack) < ab_trouter::observation(Activity::Reported));
}

#[test]
fn every_own_class_says_what_it_is_and_why_it_isnt_needed() {
    assert!(!TROUTER_RUN_REQUESTS.is_empty());
    for entry in TROUTER_RUN_REQUESTS {
        assert!(!entry.class.is_empty() && !entry.reason.is_empty());
        assert!(entry.path.starts_with('/'), "{}", entry.class);
    }
}

#[test]
fn denied_delivery_responses_are_expected_trouter_frames() -> TestResult {
    let policy = ab_trouter::trouter_policy(Some(AB_CHAT))?;
    let denied = FrameDenial {
        url: "wss://pub-ent-zz-01-t.trouter.teams.microsoft.com/v4/c".to_owned(),
        cause: DenyCause::Unclassified,
    };
    assert!(policy.expects_frame(&denied));
    Ok(())
}
