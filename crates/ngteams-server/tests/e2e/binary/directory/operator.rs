//! The operator's profile commands on the real binary while clients run: card fields set and cleared, a manager (a
//! loop refused), out of office, a picture (its size and type limits; one taken away is gone from the store too),
//! and each change as the clients read it.

use std::io::Cursor;

use serde_json::{Value, json};
use teams_core::engine::OwnStatus;
use teams_proto::{GRAPH_AUDIENCE, GraphPicture, LokiOrganization, ManualAvailability, Mri, ShortProfileLookup};

use super::super::media::support::Stage;
use super::super::support::Node;
use super::support::{Seat, bearer, manual_row};
use crate::support::backends;

/// `format`'s encoding of a `width` by `height` picture.
fn picture(width: u32, height: u32, format: image::ImageFormat) -> Vec<u8> {
    let pixels = image::RgbaImage::from_fn(width, height, |x, y| {
        image::Rgba([u8::try_from(x % 256).unwrap(), u8::try_from(y % 256).unwrap(), 200, 255])
    });
    let mut bytes = Vec::new();
    let dynamic = image::DynamicImage::ImageRgba8(pixels);
    let dynamic = if format == image::ImageFormat::Jpeg {
        image::DynamicImage::ImageRgb8(dynamic.to_rgb8())
    } else {
        dynamic
    };
    dynamic.write_to(&mut Cursor::new(&mut bytes), format).unwrap();
    bytes
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffff_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn chunk(png: &mut Vec<u8>, kind: [u8; 4], data: &[u8]) {
    png.extend_from_slice(&u32::try_from(data.len()).unwrap().to_be_bytes());
    let mut typed = kind.to_vec();
    typed.extend_from_slice(data);
    png.extend_from_slice(&typed);
    png.extend_from_slice(&crc32(&typed).to_be_bytes());
}

/// A small PNG file whose header says 60000 by 60000 pixels: a picture no server should try to unpack whole.
fn huge_png() -> Vec<u8> {
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::new();
    header.extend_from_slice(&60_000_u32.to_be_bytes());
    header.extend_from_slice(&60_000_u32.to_be_bytes());
    header.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut png, *b"IHDR", &header);
    // one stored deflate block of zero rows
    let row = vec![0_u8; 1 + 60_000 * 3];
    let mut data = vec![0x78, 0x01, 0x00];
    let block = &row[..65_535.min(row.len())];
    let len = u16::try_from(block.len()).unwrap();
    data.extend_from_slice(&len.to_le_bytes());
    data.extend_from_slice(&(!len).to_le_bytes());
    data.extend_from_slice(block);
    chunk(&mut png, *b"IDAT", &data);
    chunk(&mut png, *b"IEND", &[]);
    png
}

struct Desk {
    stage: Stage,
    files: tempfile::TempDir,
}

impl Desk {
    fn node(&self) -> &Node {
        &self.stage.rig.first
    }

    fn file(&self, name: &str, bytes: &[u8]) -> String {
        let path = self.files.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        path.to_str().unwrap().to_owned()
    }

    fn run(&self, args: &[&str]) -> String {
        self.node().cli(args, "")
    }

    /// A command that is refused: what it said.
    fn refused(&self, args: &[&str]) -> String {
        let (done, said) = self.node().try_cli(args, "");
        assert!(!done, "{args:?} was taken: {said}");
        assert!(!said.contains('\u{1b}'), "{said:?}");
        said
    }

    /// The keys of `oid`'s pictures in the store.
    async fn pictures(&self, oid: &str) -> Vec<String> {
        let keys = self.stage.blobs.keys().await;
        keys.into_iter()
            .filter(|key| key.starts_with(&format!("profile-pictures/{oid}/")))
            .collect()
    }
}

async fn graph_user(node: &Node, token: &str, id: &str) -> Value {
    let response = node
        .http
        .get(format!("{}/graph/v1.0/users/{id}", node.endpoint))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    response.json().await.unwrap()
}

/// The running clients and the operator's desk.
struct Scene {
    desk: Desk,
    a: Seat,
    b: Seat,
    c_mri: String,
    client: teams_proto::Client<super::support::Transport>,
    graph: String,
}

/// Every card field set on the command line, as the clients read it.
async fn fields(scene: &Scene) {
    let Scene { desk, a, b, .. } = scene;
    // a card no field is set on yet
    let card = a.engine().profile_card(b.mri.clone()).await.unwrap();
    assert_eq!((card.job_title.clone(), card.office.clone()), (None, None));

    let set = desk.run(&[
        "profile",
        "set",
        "b@own.test",
        "--given-name",
        "Bea",
        "--surname",
        "Example",
        "--job-title",
        "Staff Engineer",
        "--department",
        "R&D",
        "--office",
        "Building 7 / Room 4",
        "--company",
        "Own Ltd",
        "--mobile-phone",
        "+1 555 0100",
        "--business-phone",
        "+1 555 0101",
        "--manager",
        "c@own.test",
        "--out-of-office",
        "true",
    ]);
    assert_eq!(set, "updated b@own.test\n");
    let card = a.engine().profile_card(b.mri.clone()).await.unwrap();
    assert_eq!(card.job_title.as_deref(), Some("Staff Engineer"));
    assert_eq!(card.office.as_deref(), Some("Building 7 / Room 4"));
    assert_eq!(card.out_of_office, Some(true));
    assert_eq!(card.presence.as_ref().and_then(|p| p.out_of_office), Some(true));
}

/// The same fields in Graph's record, the short profile, the reporting line and `profile show`.
async fn record(scene: &Scene) {
    let Scene {
        desk,
        b,
        client,
        graph,
        c_mri,
        ..
    } = scene;
    let rig = &desk.stage.rig;
    let record = graph_user(rig.other(), graph, b.oid()).await;
    assert_eq!(
        record,
        json!({
            "@odata.context": format!("{}/graph/v1.0/$metadata#users/$entity", rig.origin),
            "id": b.oid(), "displayName": "QA Operator", "userPrincipalName": "b@own.test", "mail": "b@own.test",
            "businessPhones": ["+1 555 0101"], "givenName": "Bea", "surname": "Example",
            "jobTitle": "Staff Engineer", "mobilePhone": "+1 555 0100", "officeLocation": "Building 7 / Room 4",
            "preferredLanguage": null,
        })
    );
    let lookup = ShortProfileLookup::new([Mri::new(b.mri.clone())]).unwrap();
    let answer = client.post_read(lookup.clone()).await.unwrap();
    let short = lookup.read_answer(answer.body()).unwrap().remove(0);
    assert_eq!(short.job_title.as_deref(), Some("Staff Engineer"));
    let raw: Value = serde_json::from_slice(answer.body()).unwrap();
    assert_eq!(raw["value"][0]["department"], "R&D");
    assert_eq!(raw["value"][0]["companyName"], "Own Ltd");
    let line = client.read(&LokiOrganization::of(&b.mri).unwrap()).await.unwrap();
    let line = LokiOrganization::read_answer(line.body()).unwrap();
    assert_eq!(line.managers.len(), 1);
    assert_eq!(format!("8:orgid:{}", line.managers[0].object_id), *c_mri);
    let shown = desk.run(&["profile", "show", "b@own.test"]);
    for row in [
        "job title\tStaff Engineer\n",
        "manager\tc@own.test\n",
        "out of office\tyes\n",
        "picture\tno\n",
    ] {
        assert!(shown.contains(row), "{row:?} not in {shown:?}");
    }
}

/// Cleared fields go; the others stay.
async fn cleared(scene: &Scene) {
    let Scene { desk, a, b, graph, .. } = scene;
    let rig = &desk.stage.rig;
    // cleared fields are gone from the card and the record; the others stay
    desk.run(&[
        "profile",
        "set",
        "b@own.test",
        "--job-title",
        "",
        "--out-of-office",
        "false",
    ]);
    let card = a.engine().profile_card(b.mri.clone()).await.unwrap();
    assert_eq!(card.job_title, None);
    assert_eq!(card.office.as_deref(), Some("Building 7 / Room 4"));
    assert_eq!(card.out_of_office, Some(false));
    let record = graph_user(rig.other(), graph, b.oid()).await;
    assert_eq!(record["jobTitle"], Value::Null);
}

/// A reporting loop and other nonsense are refused.
async fn loops(scene: &Scene) {
    let Scene { desk, b, client, .. } = scene;
    // a reporting loop is refused, directly, around and through a third
    desk.run(&["profile", "set", "c@own.test", "--manager", "d@own.test"]);
    for (who, manager) in [
        ("d@own.test", "b@own.test"),
        ("c@own.test", "b@own.test"),
        ("b@own.test", "b@own.test"),
    ] {
        let said = desk.refused(&["profile", "set", who, "--manager", manager]);
        eprintln!("{who} --manager {manager}: {said}");
    }
    let line = client.read(&LokiOrganization::of(&b.mri).unwrap()).await.unwrap();
    let line = LokiOrganization::read_answer(line.body()).unwrap();
    assert_eq!(line.managers.len(), 2, "b → c → d, unchanged");
    for (args, what) in [
        (
            vec!["profile", "set", "b@own.test", "--office", &"x".repeat(257)],
            "a value too long",
        ),
        (
            vec!["profile", "set", "b@own.test", "--manager", "nobody@own.test"],
            "no such manager",
        ),
        (
            vec!["profile", "set", "b@own.test", "--out-of-office", "maybe"],
            "not a flag",
        ),
        (
            vec!["profile", "set", "nobody@own.test", "--job-title", "Ghost"],
            "no such account",
        ),
    ] {
        let said = desk.refused(&args);
        eprintln!("{what}: {said}");
    }
}

/// The operator's PNG, kept as one 96x96 JPEG: its store keys.
async fn first_picture(scene: &Scene) -> Vec<String> {
    let Scene { desk, b, client, .. } = scene;
    let rig = &desk.stage.rig;
    // a picture: the operator's PNG, cut square and kept as one 96x96 JPEG
    assert_eq!(desk.pictures(b.oid()).await, Vec::<String>::new());
    let png = desk.file("b.png", &picture(300, 200, image::ImageFormat::Png));
    let set = desk.run(&["profile", "picture", "b@own.test", &png]);
    assert!(set.starts_with("picture set for b@own.test ("), "{set}");
    let first = desk.pictures(b.oid()).await;
    assert_eq!(first.len(), 1, "{first:?}");
    let read = GraphPicture::of(&b.mri).unwrap();
    let served = read.read_answer(&client.read(&read).await.unwrap()).unwrap();
    let decoded = image::load_from_memory(&served.bytes).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (96, 96));
    assert_eq!(served.bytes, desk.stage.blobs.bytes(&first[0]).await);
    // a client that starts now draws it
    let mut fresh = Seat::sign_in(rig.other(), &rig.origin, "a@own.test").await;
    fresh.engine().person(b.mri.clone()).await.unwrap();
    fresh.idle_for(std::time::Duration::from_secs(2)).await;
    let drawn = fresh.engine().person(b.mri.clone()).await.unwrap();
    assert_eq!(drawn.picture.map(|p| p.bytes), Some(served.bytes.clone()));
    fresh.close();
    first
}

/// Each type replaces the one before: the store keys after.
async fn picture_types(scene: &Scene, first: Vec<String>) -> Vec<String> {
    let Scene { desk, b, .. } = scene;
    // every type the command takes replaces the one before, which leaves the store
    let mut before = first;
    for (name, format) in [
        ("b.jpg", image::ImageFormat::Jpeg),
        ("b.gif", image::ImageFormat::Gif),
        ("b.webp", image::ImageFormat::WebP),
    ] {
        let file = desk.file(name, &picture(64, 400, format));
        let set = desk.run(&["profile", "picture", "b@own.test", &file]);
        assert!(set.starts_with("picture set for b@own.test ("), "{name}: {set}");
        let now = desk.pictures(b.oid()).await;
        assert_eq!(now.len(), 1, "{name}: {now:?}");
        assert_ne!(now, before, "{name}");
        before = now;
    }
    before
}

/// What the picture command refuses leaves the kept picture as it was.
async fn refusals(scene: &Scene, before: &[String]) {
    let Scene { desk, b, client, .. } = scene;
    let read = GraphPicture::of(&b.mri).unwrap();
    // refused: no picture, too large, a header claiming a huge picture; the kept one stays
    let mut padded = picture(10, 10, image::ImageFormat::Png);
    padded.resize(4 * 1024 * 1024 + 1, 0);
    let refusals = [
        ("text", desk.file("notes.txt", b"not a picture")),
        ("too large", desk.file("big.png", &padded)),
        ("huge header", desk.file("huge.png", &huge_png())),
        (
            "svg",
            desk.file(
                "b.svg",
                b"<svg xmlns='http://www.w3.org/2000/svg' width='9' height='9'/>",
            ),
        ),
        ("empty", desk.file("empty.png", b"")),
    ];
    for (what, file) in &refusals {
        let said = desk.refused(&["profile", "picture", "b@own.test", file]);
        eprintln!("picture {what}: {said}");
        assert_eq!(desk.pictures(b.oid()).await, before, "{what}");
    }
    let exactly = {
        let mut bytes = picture(10, 10, image::ImageFormat::Png);
        bytes.resize(4 * 1024 * 1024, 0);
        desk.file("exactly.png", &bytes)
    };
    let (done, said) = desk.node().try_cli(&["profile", "picture", "b@own.test", &exactly], "");
    assert!(done, "a picture of exactly 4 MiB is taken: {said}");
    let taken = desk.pictures(b.oid()).await;
    assert_eq!(taken.len(), 1, "{taken:?}");
    assert_ne!(taken, before, "the picture of exactly 4 MiB replaced the one before");
    assert!(
        client.read(&read).await.is_ok(),
        "the server still serves after the refusals"
    );
}

/// Taken away: not served, gone from the store too.
async fn taken_away(scene: &Scene) {
    let Scene { desk, b, client, .. } = scene;
    let read = GraphPicture::of(&b.mri).unwrap();
    // taken away: not served, gone from the store too
    assert_eq!(
        desk.run(&["profile", "remove-picture", "b@own.test"]),
        "picture removed for b@own.test\n"
    );
    assert!(client.read(&read).await.is_err());
    assert_eq!(desk.pictures(b.oid()).await, Vec::<String>::new());
    assert_eq!(
        desk.run(&["profile", "remove-picture", "b@own.test"]),
        "b@own.test has no picture\n"
    );
}

/// The operator deletes b: card, picture and presence show nobody, and the store keeps nothing of b.
async fn deleted(scene: &mut Scene) {
    let Scene { desk, a, b, client, .. } = scene;
    let read = GraphPicture::of(&b.mri).unwrap();
    let file = desk.file("again.png", &picture(96, 96, image::ImageFormat::Png));
    desk.run(&["profile", "picture", "b@own.test", &file]);
    assert_eq!(desk.pictures(b.oid()).await.len(), 1);
    b.report(OwnStatus::Manual(ManualAvailability::Busy)).await;
    let database = &desk.stage.rig.database;
    assert!(manual_row(database, b.oid()).await.is_some());
    let (b_mri, b_oid) = (b.mri.clone(), b.oid().to_owned());
    b.close();
    desk.run(&["user", "delete", "b@own.test"]);
    assert!(client.read(&read).await.is_err());
    assert_eq!(desk.pictures(&b_oid).await, Vec::<String>::new());
    assert_eq!(manual_row(database, &b_oid).await, None, "b's status set by hand stays");
    shows_nobody(a, &b_mri).await;
}

/// `a` reads no presence of `mri`, and its engine shows an empty card (or none): no name, title, office or presence.
async fn shows_nobody(a: &Seat, mri: &str) {
    assert_eq!(a.read(&[mri]).await[0].availability, None);
    if let Ok(card) = a.engine().profile_card(mri.to_owned()).await {
        assert_eq!((card.name, card.job_title, card.office), (None, None, None));
        assert_eq!(card.presence.map(|p| p.availability), None);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_operators_profile_commands_reach_the_running_clients() {
    for backend in backends() {
        let desk = Desk {
            stage: Stage::start(backend, "").await,
            files: tempfile::tempdir().unwrap(),
        };
        let rig = &desk.stage.rig;
        let a = Seat::sign_in(rig.other(), &rig.origin, "a@own.test").await;
        let b = Seat::sign_in(&rig.first, &rig.origin, "b@own.test").await;
        let c_mri = Seat::sign_in(&rig.first, &rig.origin, "c@own.test").await.mri.clone();
        let client = a.client().await;
        let graph = bearer(rig.other(), "a@own.test", GRAPH_AUDIENCE).await;
        let mut scene = Scene {
            desk,
            a,
            b,
            c_mri,
            client,
            graph,
        };
        fields(&scene).await;
        record(&scene).await;
        cleared(&scene).await;
        loops(&scene).await;
        let first = first_picture(&scene).await;
        let kept = picture_types(&scene, first).await;
        refusals(&scene, &kept).await;
        taken_away(&scene).await;
        deleted(&mut scene).await;
        scene.a.close();
        scene.desk.stage.stop().await;
    }
}
