//! The `ab-image` scenario's offline parts: its test image is a well-formed PNG it makes itself, its forced denies take
//! every write to an AMS object on any host and no read, and its deny policy expects those and the probes only.

use std::io::Read as _;

use proto_lab::classify::Classification;
use proto_lab::fence_http::{Denial, DenyCause, ForcedDeny, PausedRequest};
use proto_lab::scenario::ScenarioName;
use proto_lab::scenario::ab_chat::{EARLIER_RUN_PROBE, THIS_RUN_PROBE};
use proto_lab::scenario::ab_image::{self, MEDIA_WRITE, ObjectMode, Texts, media_rules, object_id, test_png};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const CHAT: &str = "19:0000000a-0000-4000-8000-000000000001_0000000b-0000-4000-8000-000000000002@unq.gbl.spaces";

/// The chunks of `png` after its signature, each CRC checked: (kind, data).
fn chunks(png: &[u8]) -> TestResult<Vec<(String, Vec<u8>)>> {
    let rest = png.strip_prefix(b"\x89PNG\r\n\x1a\n").ok_or("no PNG signature")?;
    let mut chunks = Vec::new();
    let mut at = 0;
    while at < rest.len() {
        let length = usize::try_from(u32::from_be_bytes(rest[at..at + 4].try_into()?))?;
        let kind = &rest[at + 4..at + 8];
        let data = &rest[at + 8..at + 8 + length];
        let crc = u32::from_be_bytes(rest[at + 8 + length..at + 12 + length].try_into()?);
        let mut check = flate2::Crc::new();
        check.update(kind);
        check.update(data);
        assert_eq!(check.sum(), crc, "the CRC of {}", String::from_utf8_lossy(kind));
        chunks.push((String::from_utf8(kind.to_vec())?, data.to_vec()));
        at += 12 + length;
    }
    Ok(chunks)
}

#[test]
fn the_test_image_is_a_well_formed_png_of_the_scenarios_own_making() -> TestResult {
    let png = test_png(0x40);
    let chunks = chunks(&png)?;
    let kinds: Vec<&str> = chunks.iter().map(|(kind, _)| kind.as_str()).collect();
    assert_eq!(kinds, ["IHDR", "IDAT", "IEND"]);
    let header = &chunks[0].1;
    assert_eq!(&header[..8], [0, 0, 0, 48, 0, 0, 0, 48], "48 × 48");
    assert_eq!(&header[8..], [8, 2, 0, 0, 0], "8-bit RGB, no interlace");
    let mut raw = Vec::new();
    flate2::read::ZlibDecoder::new(chunks[1].1.as_slice()).read_to_end(&mut raw)?;
    // each row: its filter byte, then 3 bytes a pixel
    assert_eq!(raw.len(), 48 * (1 + 48 * 3));
    assert!(raw.chunks(1 + 48 * 3).all(|row| row[0] == 0));
    // two colours in squares: the first pixel is the light one, the ninth along the dark one
    assert_eq!(&raw[1..4], [0xe8, 0xd0, 0x40]);
    assert_eq!(&raw[25..28], [0x20, 0x60, 0xa0]);
    assert_ne!(test_png(0x41), png, "the seed changes the image");
    Ok(())
}

fn request(method: &str, url: &str) -> PausedRequest {
    PausedRequest {
        request_id: "1".to_owned(),
        method: method.to_owned(),
        url: url.to_owned(),
        headers: Vec::new(),
        body: Some(br#"{"type":"pish/image","permissions":{"19:x@unq.gbl.spaces":["read"]}}"#.to_vec()),
        network_id: None,
    }
}

#[test]
fn every_write_to_an_ams_object_is_forced_denied_on_any_host_and_no_read() -> TestResult {
    let rules = media_rules()?;
    assert!(rules.iter().all(|rule| rule.label() == Some(MEDIA_WRITE)));
    let denied = |method: &str, url: &str| rules.iter().any(|rule| rule.matches(&request(method, url)));
    for (method, url) in [
        ("POST", "https://us-api.asm.skype.com/v1/objects/"),
        ("POST", "https://us-api.asm.skype.com/v1/objects"),
        (
            "PUT",
            "https://us-api.asm.skype.com/v1/objects/0-wus-d1-0000/content/imgpsh",
        ),
        (
            "put",
            "https://eu-api.asm.skype.com/v1/objects/0-weu-d1-0000/content/original",
        ),
        ("POST", "https://us-api.asm.skype.com/v1/objects/0-wus-d1-0000/forward"),
        ("DELETE", "https://us-api.asm.skype.com/v1/objects/0-wus-d1-0000"),
        ("PATCH", "https://us-api.asm.skype.com/V1/Objects/0-wus-d1-0000"),
        ("POST", "https://teams.microsoft.com/asm/amer/v1/objects/"),
        ("POST", "https://teams.cloud.microsoft/ams/v1/objects/?x=1"),
        ("POST", "https://us-prod.asyncgw.teams.microsoft.com/v1/objects/"),
    ] {
        assert!(denied(method, url), "{method} {url}");
    }
    for (method, url) in [
        (
            "GET",
            "https://us-api.asm.skype.com/v1/objects/0-wus-d1-0000/views/imgo",
        ),
        ("POST", "https://us-api.asm.skype.com/v1/skypetokenauth"),
        (
            "POST",
            "https://teams.microsoft.com/api/chatsvc/amer/v1/users/ME/conversations/x/messages",
        ),
        ("POST", "https://us-api.asm.skype.com/v1/objectsx"),
    ] {
        assert!(!denied(method, url), "{method} {url}");
    }
    Ok(())
}

#[test]
fn the_run_expects_its_media_writes_and_probes_and_nothing_on_the_chat_off_the_list() -> TestResult {
    let policy = ab_image::policy(Some(CHAT))?;
    let forced = |url: &str, label: Option<&str>| Denial {
        label: label.map(str::to_owned),
        ..Denial::new("POST", url, DenyCause::Forced)
    };
    let media = forced("https://us-api.asm.skype.com/v1/objects/", Some(MEDIA_WRITE));
    assert!(ab_image::is_media_write(&media));
    assert!(policy.expects(&media));
    for label in [THIS_RUN_PROBE, EARLIER_RUN_PROBE] {
        assert!(policy.expects(&forced(
            "https://teams.microsoft.com/api/chatsvc/amer/v1/x",
            Some(label)
        )));
    }
    assert!(!policy.expects(&forced("https://us-api.asm.skype.com/v1/objects/", None)));
    assert!(!ab_image::is_media_write(&forced("https://x/", Some(THIS_RUN_PROBE))));
    // an unclassified deny of a media write isn't the run's own: the forced rule should have taken it
    let unclassified = Denial::new(
        "POST",
        "https://us-api.asm.skype.com/v1/objects/",
        DenyCause::Unclassified,
    );
    assert!(!ab_image::is_media_write(&unclassified));
    assert!(!policy.expects(&unclassified));
    // a deny naming the chat is unexpected, the list notwithstanding
    let on_chat = Denial::new(
        "PUT",
        "https://teams.microsoft.com/api/chatsvc/amer/v1/users/ME/conversations/19%3A0000000a-0000-4000-8000-000000000001_0000000b-0000-4000-8000-000000000002%40unq.gbl.spaces/properties",
        DenyCause::Unclassified,
    );
    assert!(!policy.expects(&on_chat));
    Ok(())
}

#[test]
fn the_texts_are_this_runs_and_apart() {
    let texts = Texts::new("n1");
    assert!(texts.plain.contains("n1") && texts.caption.contains("n1") && texts.file_name.contains("n1"));
    assert!(!texts.caption.contains(&texts.plain) && !texts.plain.contains(&texts.caption));
    assert_eq!(std::path::Path::new(&texts.file_name).extension(), Some("png".as_ref()));
    assert_eq!(ScenarioName::AbImage.as_str(), "ab-image");
}

const CREATION: &str = "https://us-prod.asyncgw.teams.microsoft.com/v1/objects/";

fn paused(method: &str, url: &str, network_id: Option<&str>) -> PausedRequest {
    PausedRequest {
        network_id: network_id.map(str::to_owned),
        ..request(method, url)
    }
}

#[test]
fn ab_image_object_spares_the_first_creation_once_and_denies_every_other_media_write() -> TestResult {
    let rules = ab_image::media_rules_creating()?;
    assert!(rules.iter().all(|rule| rule.label() == Some(MEDIA_WRITE)));
    let denied = |request: &PausedRequest| rules.iter().any(|rule| rule.matches(request));
    // before any creation: the upload, a forward, other hosts and a creation without a Network id are denied
    for (method, url) in [
        (
            "PUT",
            "https://us-prod.asyncgw.teams.microsoft.com/v1/objects/0-wus-d1-0000/content/imgpsh",
        ),
        (
            "POST",
            "https://us-prod.asyncgw.teams.microsoft.com/v1/objects/0-wus-d1-0000/forward",
        ),
        (
            "DELETE",
            "https://us-prod.asyncgw.teams.microsoft.com/v1/objects/0-wus-d1-0000",
        ),
        ("POST", "https://us-api.asm.skype.com/v1/objects/"),
        ("POST", "https://teams.microsoft.com/asm/amer/v1/objects/"),
        ("POST", "https://us-prod.asyncgw.teams.microsoft.com/v1/objects/?x=1"),
    ] {
        assert!(denied(&paused(method, url, Some("9.9"))), "{method} {url}");
    }
    assert!(denied(&paused("POST", CREATION, None)), "no Network id: never spared");
    // before sparing starts (an earlier run's image tried again on load) a creation is denied
    assert!(denied(&paused("POST", CREATION, Some("1.0"))));
    assert!(rules.iter().all(|rule| rule.spared().is_none()));
    for rule in &rules {
        rule.start_sparing();
    }
    // the first creation since is spared on both of its pauses; the next one is denied
    assert!(!denied(&paused("POST", CREATION, Some("1.1"))));
    assert!(!denied(&paused("POST", CREATION, Some("1.1"))), "its other pause");
    assert!(denied(&paused("POST", CREATION, Some("1.2"))));
    let spared: Vec<String> = rules.iter().filter_map(ForcedDeny::spared).collect();
    assert_eq!(spared, ["1.1"]);
    // ab-image itself spares nothing
    let plain = media_rules()?;
    assert!(
        plain
            .iter()
            .any(|rule| rule.matches(&paused("POST", CREATION, Some("1.1"))))
    );
    Ok(())
}

#[test]
fn the_modes_name_their_scenario_and_classifier() {
    assert_eq!(ObjectMode::Denied.scenario(), ScenarioName::AbImage);
    assert_eq!(ObjectMode::CreatedOnce.scenario(), ScenarioName::AbImageObject);
    assert_eq!(ScenarioName::AbImageObject.as_str(), "ab-image-object");
    let headers: Vec<(String, Option<String>)> = [
        ("content-type", Some("application/json")),
        ("x-ams-post-sharing-mode", Some("Inline")),
        ("x-ms-migration", Some("True")),
        ("x-ms-test-user", Some("False")),
        ("authorization", None),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_owned(), value.map(str::to_owned)))
    .collect();
    let body = format!(
        r#"{{"type":"pish/image","permissions":{{"{CHAT}":["read"]}},"sharingMode":"Inline","filename":"MicrosoftTeams-image.png"}}"#
    );
    let classify = |mode: ObjectMode, chat: Option<&str>| {
        mode.classifier(chat)
            .classify_with_headers("POST", CREATION, &headers, Some(body.as_bytes()))
    };
    assert!(matches!(
        classify(ObjectMode::CreatedOnce, Some(CHAT)),
        Classification::Write(_)
    ));
    assert_eq!(classify(ObjectMode::CreatedOnce, None), Classification::Unknown);
    assert_eq!(classify(ObjectMode::Denied, Some(CHAT)), Classification::Unknown);
}

#[test]
fn the_object_is_read_from_the_denied_uploads_url() {
    assert_eq!(
        object_id("https://us-prod.asyncgw.teams.microsoft.com/v1/objects/0-wus-d1-0000/content/imgpsh"),
        Some("0-wus-d1-0000")
    );
    assert_eq!(object_id("https://x/v1/objects/0-wus-d1-0000"), Some("0-wus-d1-0000"));
    assert_eq!(object_id(CREATION), None);
    assert_eq!(object_id("https://x/v1/other/0-wus-d1-0000"), None);
}
