//! Teams app packages that Teams would refuse, and the admin API refusing each: a bad id, long names, two bots, a
//! scope or color Teams has not, a person's or the Workflows bot's id, wrong icons, zip bombs, 6 MiB.

use std::io::{Cursor, Write as _};

use bytes::Bytes;
use reqwest::Method;
use serde_json::{Value, json};
use teams_auth::secrets::Secret;

use super::super::support::Node;
use super::support::{APPS, WORKFLOWS_BOT, call, code, upload};
use crate::bots::{manifest, package, png};

pub const PACKAGE: &str = "/api/admin/v1/apps/package";

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// A package Teams would refuse: why, the bytes, the status.
type Case = (&'static str, Bytes, u16);

/// A package of a good manifest after `change`, with good icons.
fn changed(change: impl FnOnce(&mut Value)) -> Bytes {
    let mut manifest = manifest(&new_id(), &new_id(), "1.0.0");
    change(&mut manifest);
    package(&manifest, &png(192, 192), &png(32, 32), &[])
}

fn manifest_cases(person: &str) -> Vec<Case> {
    let two_bots = json!([
        {"botId": new_id(), "scopes": ["personal"]},
        {"botId": new_id(), "scopes": ["personal"]},
    ]);
    let changes = [
        ("an app id that is not a GUID", "/id", json!("com.example.polls"), 400),
        (
            "a GUID without its dashes",
            "/id",
            json!(uuid::Uuid::new_v4().simple().to_string()),
            400,
        ),
        ("a bot id that is not a GUID", "/bots/0/botId", json!("polls-bot"), 400),
        (
            "a short name of 31 characters",
            "/name/short",
            json!("abcdefghijklmnopqrstuvwxyz12345"),
            400,
        ),
        ("an empty short name", "/name/short", json!("  "), 400),
        (
            "a full name of 101 characters",
            "/name/full",
            json!("f".repeat(101)),
            400,
        ),
        (
            "a short description of 81 characters",
            "/description/short",
            json!("d".repeat(81)),
            400,
        ),
        (
            "a developer name of 33 characters",
            "/developer/name",
            json!("v".repeat(33)),
            400,
        ),
        ("two bots", "/bots", two_bots, 400),
        (
            "a bot scope Teams has not",
            "/bots/0/scopes",
            json!(["personal", "meetings"]),
            400,
        ),
        ("a bot without scopes", "/bots/0/scopes", json!([]), 400),
        ("an accent color by name", "/accentColor", json!("red"), 400),
        ("an accent color of three digits", "/accentColor", json!("#FFF"), 400),
        (
            "an icon path out of the package",
            "/icons/color",
            json!("../../etc/passwd"),
            400,
        ),
        ("a bot that is a person", "/bots/0/botId", json!(person), 409),
        ("the Workflows bot", "/bots/0/botId", json!(WORKFLOWS_BOT), 409),
    ];
    changes
        .into_iter()
        .map(|(why, pointer, value, status)| {
            let bytes = changed(|manifest| *manifest.pointer_mut(pointer).unwrap() = value);
            (why, bytes, status)
        })
        .collect()
}

/// A zip whose first entry, `name`, inflates to `head` and then `size` bytes of `fill`; then `rest`.
fn inflating(head: &[u8], name: &str, size: usize, fill: u8, rest: &[(&str, Vec<u8>)]) -> Bytes {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    zip.start_file(name, options).unwrap();
    zip.write_all(head).unwrap();
    let chunk = vec![fill; 1 << 20];
    let mut left = size;
    while left > 0 {
        let take = left.min(chunk.len());
        zip.write_all(&chunk[..take]).unwrap();
        left -= take;
    }
    for (file, bytes) in rest {
        zip.start_file(*file, options).unwrap();
        zip.write_all(bytes).unwrap();
    }
    Bytes::from(zip.finish().unwrap().into_inner())
}

/// A package of 6 MiB: the good files and 6 MiB that don't compress, stored.
fn six_mib(manifest: &[u8], color: &[u8], outline: &[u8]) -> Bytes {
    let mut padding = vec![0_u8; 6 * 1024 * 1024];
    for (index, byte) in padding.iter_mut().enumerate() {
        *byte = u8::try_from((index * 7919 + index / 13) % 251).unwrap();
    }
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, bytes) in [
        ("manifest.json", manifest),
        ("color.png", color),
        ("outline.png", outline),
        ("padding.bin", &padding),
    ] {
        zip.start_file(name, stored).unwrap();
        zip.write_all(bytes).unwrap();
    }
    Bytes::from(zip.finish().unwrap().into_inner())
}

fn file_cases() -> Vec<Case> {
    let (color, outline) = (png(192, 192), png(32, 32));
    let good = manifest(&new_id(), &new_id(), "1.0.0");
    let bytes = serde_json::to_vec(&good).unwrap();
    let jpeg_like = b"\xff\xd8\xff\xe0 synthetic, not a PNG".to_vec();
    let mut icon_bomb = png(192, 192);
    icon_bomb.resize(64 << 20, 0);
    let icons = [("color.png", color.clone()), ("outline.png", outline.clone())];
    vec![
        ("no manifest.json", inflating(b"", "readme.txt", 10, b'x', &[]), 400),
        (
            "a manifest that is not JSON",
            inflating(b"{ not json", "manifest.json", 10, b' ', &[]),
            400,
        ),
        (
            "a color icon of 191 by 192",
            package(&good, &png(191, 192), &outline, &[]),
            400,
        ),
        (
            "a color icon of 32 by 32",
            package(&good, &png(32, 32), &outline, &[]),
            400,
        ),
        (
            "an outline icon of 32 by 33",
            package(&good, &color, &png(32, 33), &[]),
            400,
        ),
        (
            "an outline icon of 192 by 192",
            package(&good, &color, &png(192, 192), &[]),
            400,
        ),
        (
            "a color icon that is not a PNG",
            package(&good, &jpeg_like, &outline, &[]),
            400,
        ),
        (
            "a manifest that inflates to 200 MiB",
            inflating(&bytes, "manifest.json", 200 << 20, b' ', &icons),
            400,
        ),
        (
            "a color icon that inflates to 64 MiB",
            inflating(
                &bytes,
                "manifest.json",
                0,
                b' ',
                &[("color.png", icon_bomb), ("outline.png", outline.clone())],
            ),
            400,
        ),
        ("not a zip", Bytes::from_static(b"PK\x03\x04 synthetic, truncated"), 400),
        ("an empty body", Bytes::new(), 400),
        ("a package of 6 MiB", six_mib(&bytes, &color, &outline), 413),
    ]
}

/// Uploads each package Teams would refuse: each refused, saying why, and none registered.
pub async fn refused_packages(node: &Node, root: &Secret, person: &str) {
    eprintln!("registry: packages Teams would refuse");
    let count = async || {
        call(node, Method::GET, APPS, Some(root), None).await.1["apps"]
            .as_array()
            .unwrap()
            .len()
    };
    let before = count().await;
    for (why, bytes, expected) in manifest_cases(person).into_iter().chain(file_cases()) {
        let started = std::time::Instant::now();
        let (status, answer) = upload(node, root, Method::POST, PACKAGE, bytes).await;
        let message = &answer["error"]["message"];
        eprintln!("  refused {why}: {status} {message} in {:?}", started.elapsed());
        assert_eq!(status, expected, "{why}: {answer}");
        let codes: &[&str] = match expected {
            400 => &["InvalidPackage", "InvalidRequest"],
            409 => &["BotExists"],
            _ => &[""],
        };
        assert!(codes.contains(&code(&answer)), "{why}: {answer}");
    }
    assert_eq!(count().await, before, "a refused package registers nothing");
    assert_eq!(
        node.status("/health/ready").await,
        Some(204),
        "the node lived through the bombs"
    );
}
