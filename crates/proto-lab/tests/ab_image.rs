//! The `ab-image` scenario's offline parts: its test image is a well-formed PNG it makes itself, its forced denies take
//! every write to an AMS object on any host and no read, and its deny policy expects those and the probes only.

use std::io::Read as _;

use proto_lab::capture_hold::{HeldRequest, PausedRequest};
use proto_lab::scenario::ScenarioName;
use proto_lab::scenario::ab_image::{self, MEDIA_WRITE, Texts, media_rules, test_png};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

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
fn the_texts_are_this_runs_and_apart() {
    let texts = Texts::new("n1");
    assert!(texts.plain.contains("n1") && texts.caption.contains("n1") && texts.file_name.contains("n1"));
    assert!(!texts.caption.contains(&texts.plain) && !texts.plain.contains(&texts.caption));
    assert_eq!(std::path::Path::new(&texts.file_name).extension(), Some("png".as_ref()));
    assert_eq!(ScenarioName::AbImage.as_str(), "ab-image");
}

#[test]
fn media_hold_records_are_identified_by_their_label() {
    let mut request = HeldRequest::new("POST", "https://us-api.asm.skype.com/v1/objects/");
    assert!(!ab_image::is_media_write(&request));
    request.label = Some(MEDIA_WRITE.into());
    assert!(ab_image::is_media_write(&request));
}
