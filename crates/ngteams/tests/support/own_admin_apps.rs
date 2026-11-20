//! An own server's apps and bots from the app (`ngteams::own_admin`, the Manage apps page's service), against an
//! in-process `ngteams-server` over TLS: the administrator's service sees that the server serves apps, registers a
//! bot (its secret once), uploads a synthetic Teams app package, turns an app off, changes its bot's endpoint and who
//! may use it, gives its bot a new secret, reads the addresses a bot's settings name and deletes an app; the server's
//! refusals come back as the page says them; an account that isn't an administrator is refused. Every value is made
//! up and nothing leaves loopback.

use std::io::{Cursor, Write as _};

use ngteams::accounts;
use serde_json::json;
use teams_auth::admin::{AppChanges, BotAuthMode};
use teams_auth::secrets::Secret;
use teams_ui::app_admin::{AppAdmin as _, AppProblem, BotForm};
use teams_ui::user_admin::AdminProblem;

use super::own_admin::{at, service, setup_in};
use super::own_server::{OwnServer, PASSWORD};

const ADMIN: &str = "ada.apps@own.example.test";
const DANA: &str = "dana.apps@own.example.test";
const PACKAGE_APP: &str = "00000000-0000-4000-8000-0000000000e1";
const PACKAGE_BOT: &str = "00000000-0000-4000-8000-0000000000e2";

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn png(side: u32) -> TestResult<Vec<u8>> {
    let mut bytes = Vec::new();
    image::RgbaImage::from_pixel(side, side, image::Rgba([79, 82, 178, 255]))
        .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)?;
    Ok(bytes)
}

/// A synthetic Teams app package with one bot, as `docs/server.md` takes it.
fn package() -> TestResult<Vec<u8>> {
    let manifest = json!({
        "manifestVersion": "1.17",
        "version": "1.0.0",
        "id": PACKAGE_APP,
        "developer": {"name": "Synthetic Tools", "websiteUrl": "https://tools.example.test",
                      "privacyUrl": "https://tools.example.test/privacy", "termsOfUseUrl": "https://tools.example.test/terms"},
        "icons": {"color": "color.png", "outline": "outline.png"},
        "name": {"short": "Polls (synthetic)"},
        "description": {"short": "Quick polls", "full": "Synthetic polls."},
        "accentColor": "#4F6BED",
        "bots": [{"botId": PACKAGE_BOT, "scopes": ["personal"]}],
    });
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    for (name, bytes) in [
        ("manifest.json", serde_json::to_vec(&manifest)?),
        ("color.png", png(192)?),
        ("outline.png", png(32)?),
    ] {
        zip.start_file(name, options)?;
        zip.write_all(&bytes)?;
    }
    Ok(zip.finish()?.into_inner())
}

fn form(name: &str, endpoint: Option<&str>, auth_mode: BotAuthMode) -> BotForm {
    BotForm {
        name: name.to_owned(),
        description: "A synthetic bot".to_owned(),
        endpoint: endpoint.map(str::to_owned),
        auth_mode,
    }
}

type Service = ngteams::own_admin::AdminService<ngteams::secrets::AppSecrets>;

/// A bot from the form, its secret once; the list never holds it. The bot's app and its secret.
fn registered(service: &Service) -> TestResult<(teams_auth::admin::AdminApp, String)> {
    let made = service.register(&form(
        "Synthetic Helper",
        Some("https://bots.example.test/api/messages"),
        BotAuthMode::Signed,
    ))?;
    let secret = made.secret.clone().ok_or("a registered bot's secret")?;
    assert!(!format!("{made:?}").contains(&secret));
    assert!(!secret.is_empty());
    let bot = made.app.bot.clone().ok_or("a bot")?;
    assert_eq!(bot.mri, format!("28:{}", bot.id));
    let listed = service.apps()?;
    assert_eq!(listed.len(), 1);
    assert!(!format!("{listed:?}").contains(&secret));
    Ok((made.app, secret))
}

/// The server's refusals of a form and a package, said as the page says them.
fn refused(service: &Service) -> TestResult {
    let refusal = |name: &str, endpoint, mode| service.register(&form(name, endpoint, mode)).err();
    assert_eq!(refusal("", None, BotAuthMode::Signed), Some(AppProblem::InvalidName));
    assert_eq!(
        refusal("Synthetic Plain", Some("ftp://bots.example.test"), BotAuthMode::Signed),
        Some(AppProblem::InvalidEndpoint)
    );
    assert_eq!(
        refusal("Synthetic Loose", None, BotAuthMode::Unsigned),
        Some(AppProblem::UnsignedNotAllowed)
    );
    let uploaded = service.upload(package()?)?;
    assert_eq!(uploaded.app.id, PACKAGE_APP);
    assert!(uploaded.secret.is_some());
    assert_eq!(service.upload(package()?).err(), Some(AppProblem::AppExists));
    assert!(matches!(
        service.upload(b"not a zip".to_vec()).err(),
        Some(AppProblem::InvalidPackage(_))
    ));
    Ok(())
}

/// Off, a new endpoint, one person only; an unknown person refused; the people offered are no bots.
fn changed(service: &Service, id: &str, dana: &str) -> TestResult {
    let changes = AppChanges {
        enabled: Some(false),
        everyone: Some(false),
        users: Some(vec![dana.to_owned()]),
        endpoint: Some("https://bots.example.test/v2".to_owned()),
        ..AppChanges::default()
    };
    let answered = service.change(id, &changes)?;
    assert!(!answered.enabled && !answered.everyone);
    assert_eq!(answered.users, [dana]);
    let endpoint = answered.bot.and_then(|bot| bot.endpoint);
    assert_eq!(endpoint.as_deref(), Some("https://bots.example.test/v2"));
    let unknown = AppChanges {
        users: Some(vec!["00000000-0000-4000-8000-00000000dead".to_owned()]),
        ..AppChanges::default()
    };
    assert_eq!(service.change(id, &unknown).err(), Some(AppProblem::UnknownUser));
    let people = service.people("", None)?;
    assert!(people.users.iter().any(|user| user.upn == DANA));
    assert!(people.users.iter().all(|user| user.id != id));
    Ok(())
}

#[test]
fn an_administrator_registers_and_manages_the_servers_apps() -> TestResult {
    let server = OwnServer::start()?;
    let dir = tempfile::tempdir()?;
    let setup = setup_in(&dir);
    server.add_admin(ADMIN, "Ada Admin")?;
    let dana = server.add_user(DANA, "Dana Doe")?;
    let admin = accounts::add_own(&setup, ADMIN, &at(&server), &Secret::new(PASSWORD))?;
    let service = service(&setup, &admin)?;
    assert!(service.serves_apps()?);
    assert!(service.apps()?.is_empty());

    let (app, secret) = registered(&service)?;
    refused(&service)?;
    changed(&service, &app.id, &dana)?;

    // a new secret, the addresses a bot's settings name
    let rotated = service.new_secret(&app.id)?;
    assert_ne!(rotated, secret);
    let connection = service.connection()?;
    assert_eq!(connection.origin, server.origin);
    assert!(connection.issuer.starts_with(&server.origin), "{}", connection.issuer);

    // deleted, then gone
    service.delete(&app.id)?;
    assert_eq!(service.delete(&app.id).err(), Some(AppProblem::NotFound));
    assert_eq!(service.apps()?.len(), 1);
    Ok(())
}

#[test]
fn an_account_that_isnt_an_administrator_is_refused_the_apps() -> TestResult {
    let server = OwnServer::start()?;
    let dir = tempfile::tempdir()?;
    let setup = setup_in(&dir);
    server.add_user(DANA, "Dana Doe")?;
    let dana = accounts::add_own(&setup, DANA, &at(&server), &Secret::new(PASSWORD))?;
    let service = service(&setup, &dana)?;
    let demoted = service.demoted();
    assert!(!service.is_admin()?);
    assert_eq!(service.apps().unwrap_err(), AppProblem::Account(AdminProblem::NotAdmin));
    assert!(*demoted.borrow());
    assert_eq!(service.serves_apps().unwrap_err(), AdminProblem::NotAdmin);
    Ok(())
}
