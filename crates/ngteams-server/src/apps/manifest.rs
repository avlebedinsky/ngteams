//! A Teams app package: a zip with `manifest.json` and the two icons it names, checked as Teams checks an upload
//! (the subset of the manifest schema the server uses: ids, names, descriptions, the accent colour, one bot and its
//! scopes, and the icons' format and size). The manifest is kept whole, so what the server doesn't read yet (message
//! extensions, command lists, tabs) is there for the routes that will.

use std::io::{Cursor, Read as _};

use bytes::Bytes;
use serde::Deserialize;
use serde_json::Value;

use super::AppError;
use super::repo::AppRow;

/// The largest package taken, in bytes.
pub const MAX_PACKAGE: usize = 5 * 1024 * 1024;
/// The largest file read out of a package.
const MAX_ENTRY: u64 = 1024 * 1024;
/// The icons' sizes, as Teams requires them.
const COLOR: u32 = 192;
const OUTLINE: u32 = 32;
/// The scopes a bot may name.
const SCOPES: [&str; 3] = ["personal", "team", "groupChat"];

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    id: String,
    version: String,
    developer: Developer,
    name: Names,
    description: Descriptions,
    icons: Icons,
    accent_color: String,
    #[serde(default)]
    bots: Vec<Bot>,
}

#[derive(Deserialize)]
struct Developer {
    name: String,
}

#[derive(Deserialize)]
struct Names {
    short: String,
    full: Option<String>,
}

#[derive(Deserialize)]
struct Descriptions {
    short: String,
    full: String,
}

#[derive(Deserialize)]
struct Icons {
    color: String,
    outline: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Bot {
    bot_id: String,
    #[serde(default)]
    scopes: Vec<String>,
}

/// The bot a package names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageBot {
    pub id: String,
    pub scopes: Vec<String>,
}

/// A package as the registry takes it.
#[derive(Clone, Debug)]
pub struct Package {
    pub id: String,
    pub version: String,
    pub name: String,
    pub full_name: String,
    pub short_description: String,
    pub full_description: String,
    pub developer: String,
    pub accent_color: String,
    pub manifest: Value,
    pub bot: Option<PackageBot>,
    pub color: Bytes,
    pub outline: Bytes,
}

impl Package {
    /// The app's row, a new one at `at` (an update keeps the times and state it had).
    #[must_use]
    pub fn row(&self, at: i64) -> AppRow {
        AppRow {
            id: self.id.clone(),
            version: self.version.clone(),
            name: self.name.clone(),
            full_name: self.full_name.clone(),
            short_description: self.short_description.clone(),
            full_description: self.full_description.clone(),
            developer: self.developer.clone(),
            accent_color: self.accent_color.clone(),
            manifest: self.manifest.to_string(),
            source: "package".to_owned(),
            icons: true,
            everyone: true,
            enabled: true,
            created_at: at,
            updated_at: at,
        }
    }
}

fn refused(why: impl Into<String>) -> AppError {
    AppError::Package(why.into())
}

/// The package in `bytes`, checked; read on the blocking pool.
///
/// # Errors
/// [`AppError::TooLarge`], or [`AppError::Package`] saying what is wrong.
pub async fn read(bytes: Bytes) -> Result<Package, AppError> {
    if bytes.len() > MAX_PACKAGE {
        return Err(AppError::TooLarge);
    }
    tokio::task::spawn_blocking(move || read_now(&bytes))
        .await
        .map_err(|_| refused("the package can't be read"))?
}

fn read_now(bytes: &[u8]) -> Result<Package, AppError> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| refused("it is not a zip file"))?;
    let raw = entry(&mut zip, "manifest.json")?;
    let manifest: Value = serde_json::from_slice(&raw).map_err(|_| refused("manifest.json is not JSON"))?;
    let typed: Manifest =
        serde_json::from_value(manifest.clone()).map_err(|error| refused(format!("manifest.json: {error}")))?;
    let package = checked(&typed, manifest)?;
    let color = icon(&mut zip, &typed.icons.color, COLOR)?;
    let outline = icon(&mut zip, &typed.icons.outline, OUTLINE)?;
    Ok(Package {
        color,
        outline,
        ..package
    })
}

/// One file of the package, at most [`MAX_ENTRY`] bytes.
fn entry(zip: &mut zip::ZipArchive<Cursor<&[u8]>>, name: &str) -> Result<Vec<u8>, AppError> {
    let file = zip
        .by_name(name)
        .map_err(|_| refused(format!("the package has no {name}")))?;
    let mut read = Vec::new();
    file.take(MAX_ENTRY + 1)
        .read_to_end(&mut read)
        .map_err(|_| refused(format!("{name} can't be read")))?;
    if read.len() as u64 > MAX_ENTRY {
        return Err(refused(format!("{name} is bigger than {MAX_ENTRY} bytes")));
    }
    Ok(read)
}

/// The icon `name`: a PNG of `size` by `size`.
fn icon(zip: &mut zip::ZipArchive<Cursor<&[u8]>>, name: &str, size: u32) -> Result<Bytes, AppError> {
    let bytes = entry(zip, name)?;
    let reader = image::ImageReader::with_format(Cursor::new(&bytes), image::ImageFormat::Png);
    let dimensions = reader
        .into_dimensions()
        .map_err(|_| refused(format!("{name} is not a PNG")))?;
    if dimensions != (size, size) {
        return Err(refused(format!("{name} is not {size} by {size} pixels")));
    }
    image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
        .map_err(|_| refused(format!("{name} is not a PNG")))?;
    Ok(Bytes::from(bytes))
}

fn guid(id: &str) -> Option<String> {
    uuid::Uuid::parse_str(id)
        .ok()
        .filter(|_| id.len() == 36)
        .map(|id| id.to_string())
}

/// `text` trimmed, when it has 1 to `max` characters.
fn text(text: &str, max: usize, what: &str) -> Result<String, AppError> {
    let text = text.trim();
    if text.is_empty() || text.chars().count() > max {
        return Err(refused(format!("{what} must have 1 to {max} characters")));
    }
    Ok(text.to_owned())
}

/// The manifest's fields as Teams checks them; the icons not yet read.
fn checked(manifest: &Manifest, whole: Value) -> Result<Package, AppError> {
    let id = guid(&manifest.id).ok_or_else(|| refused("id must be a GUID"))?;
    let version = text(&manifest.version, 256, "version")?;
    let name = text(&manifest.name.short, super::MAX_NAME, "name.short")?;
    let full_name = match manifest.name.full.as_deref() {
        Some(full) if !full.trim().is_empty() => text(full, 100, "name.full")?,
        _ => name.clone(),
    };
    let accent = &manifest.accent_color;
    if !(accent.len() == 7 && accent.starts_with('#') && accent[1..].bytes().all(|byte| byte.is_ascii_hexdigit())) {
        return Err(refused("accentColor must be #RRGGBB"));
    }
    if manifest.bots.len() > 1 {
        return Err(refused("an app has at most one bot"));
    }
    let bot = manifest.bots.first().map(checked_bot).transpose()?;
    Ok(Package {
        id,
        version,
        name,
        full_name,
        short_description: text(&manifest.description.short, 80, "description.short")?,
        full_description: text(&manifest.description.full, 4000, "description.full")?,
        developer: text(&manifest.developer.name, 32, "developer.name")?,
        accent_color: accent.clone(),
        manifest: whole,
        bot,
        color: Bytes::new(),
        outline: Bytes::new(),
    })
}

fn checked_bot(bot: &Bot) -> Result<PackageBot, AppError> {
    let id = guid(&bot.bot_id).ok_or_else(|| refused("bots[0].botId must be a GUID"))?;
    if bot.scopes.is_empty() || bot.scopes.iter().any(|scope| !SCOPES.contains(&scope.as_str())) {
        return Err(refused("bots[0].scopes must be some of personal, team and groupChat"));
    }
    Ok(PackageBot {
        id,
        scopes: bot.scopes.clone(),
    })
}
