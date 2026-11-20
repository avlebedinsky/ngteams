//! A picture of the user's own for each account. Where the app shows which account it is (the rail, the title bar,
//! the account menu, Settings > Accounts), it shows this picture in place of the account's profile picture. Thus two
//! accounts with the same profile picture look different. Messages, people cards and calls keep the profile picture.
//!
//! The picture is local: the app never sends it to Teams or to an own server, and it never changes the profile
//! picture of the account. [`AccountPictures::save`] decodes the file that the user chose (PNG, JPEG, the first frame
//! of a GIF, WebP), crops its centre to a square (the avatar draws it in a circle) and keeps a PNG copy in [`DIR`]
//! beside the settings file. The copy stays when the chosen file goes. The file of an account removed goes with its
//! own settings ([`crate::settings::SettingsModel::forget_account`]).
//!
//! Each save, reset and load of an account's picture has a [`Turn`]. Only the last turn of an account can change its
//! file: a save of an earlier turn that ends after a later save, a reset or a removal keeps nothing. Thus the last
//! choice of the user always stays, also when a large picture takes long to read.

use std::collections::HashMap;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};

use image::{DynamicImage, ImageReader};

/// The directory of the account pictures, beside the settings file.
pub const DIR: &str = "account-pictures";
/// The side of a kept picture, px: two times the largest avatar that shows it.
pub const SIDE: u32 = 256;
/// The largest file the app reads as a picture.
const MAX_FILE: u64 = 64 * 1024 * 1024;

/// Why a picture was not kept.
#[derive(Debug, thiserror::Error)]
pub enum PictureError {
    #[error("the file is {0} bytes, more than the {MAX_FILE} read")]
    TooLarge(u64),
    #[error("the file can't be opened: {0}")]
    Unopened(io::Error),
    #[error("the file is not a picture that can be read: {0}")]
    Unreadable(String),
    #[error("the picture can't be kept: {0}")]
    Unsaved(#[from] io::Error),
}

/// One save, reset or load of an account's picture ([`AccountPictures::turn`]); see the module docs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Turn(u64);

/// The last turn of each picture file, by its path. It is one for the process, because each [`AccountPictures`] on the
/// same directory changes the same files. Its lock is held while a file is renamed or deleted.
static TURNS: LazyLock<Mutex<HashMap<PathBuf, u64>>> = LazyLock::new(Mutex::default);

fn turns() -> MutexGuard<'static, HashMap<PathBuf, u64>> {
    TURNS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The pictures of the accounts, one file for each account in a directory (see the module docs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountPictures {
    dir: PathBuf,
}

impl AccountPictures {
    /// The pictures kept in `dir`.
    #[must_use]
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// The file of the picture of `account` (its MRI).
    #[must_use]
    pub fn path(&self, account: &str) -> PathBuf {
        self.dir.join(format!("{}.png", file_stem(account)))
    }

    /// The PNG of the picture of `account`; `None` when the user did not choose one.
    #[must_use]
    pub fn load(&self, account: &str) -> Option<Vec<u8>> {
        std::fs::read(self.path(account)).ok()
    }

    /// A new turn for the picture of `account`. A save of an earlier turn that did not end yet then keeps nothing.
    #[must_use]
    pub fn turn(&self, account: &str) -> Turn {
        let mut turns = turns();
        let last = turns.entry(self.path(account)).or_default();
        *last += 1;
        Turn(*last)
    }

    /// Whether `turn` is the last turn of the picture of `account`.
    #[must_use]
    pub fn is_current(&self, account: &str, turn: Turn) -> bool {
        turns().get(&self.path(account)) == Some(&turn.0)
    }

    /// Keeps the picture at `source` as the picture of `account`, in place of the one before, when `turn` is still
    /// its last turn ([`Self::turn`]). Returns the PNG kept; `None` when a later turn came first and nothing was kept.
    ///
    /// # Errors
    /// When `source` can't be opened, is too large or is not a picture, or the copy can't be written. The picture
    /// before stays then.
    pub fn save(&self, account: &str, source: &Path, turn: Turn) -> Result<Option<Vec<u8>>, PictureError> {
        let picture = read(source)?;
        let png = encode(&square(&picture))?;
        teams_platform::private::create_dir_all(&self.dir)?;
        let path = self.path(account);
        // written beside it with a name of its own turn, then renamed: a write that fails leaves the picture before as
        // it was, and two saves never write the same file
        let partial = path.with_extension(format!("png.{}.part", turn.0));
        let kept = write(&partial, &png).and_then(|()| {
            let turns = turns();
            if turns.get(&path) != Some(&turn.0) {
                return Ok(false);
            }
            std::fs::rename(&partial, &path).map(|()| true)
        });
        match kept {
            Ok(true) => Ok(Some(png)),
            Ok(false) => {
                let _ = std::fs::remove_file(&partial);
                Ok(None)
            }
            Err(error) => {
                let _ = std::fs::remove_file(&partial);
                Err(error.into())
            }
        }
    }

    /// Deletes the picture of `account`, so its profile picture shows again. Nothing when it has none. It takes a new
    /// turn first, so a save that did not end yet keeps nothing after it.
    ///
    /// # Errors
    /// When the file is there and can't be deleted.
    pub fn reset(&self, account: &str) -> io::Result<()> {
        let path = self.path(account);
        let mut turns = turns();
        *turns.entry(path.clone()).or_default() += 1;
        match std::fs::remove_file(&path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }
}

/// The name of a file for `account`: its letters, digits and `-` as they are, each other byte as `_` and two hex
/// digits. Thus two accounts never get the same name, and the name is valid on each system (an MRI has `:`).
#[must_use]
pub fn file_stem(account: &str) -> String {
    account
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || byte == b'-' {
                char::from(byte).to_string()
            } else {
                format!("_{byte:02x}")
            }
        })
        .collect()
}

fn read(source: &Path) -> Result<DynamicImage, PictureError> {
    let size = std::fs::metadata(source).map_err(PictureError::Unopened)?.len();
    if size > MAX_FILE {
        return Err(PictureError::TooLarge(size));
    }
    ImageReader::open(source)
        .and_then(ImageReader::with_guessed_format)
        .map_err(PictureError::Unopened)?
        .decode()
        .map_err(|error| PictureError::Unreadable(error.to_string()))
}

/// The centre of `picture` as a square, made [`SIDE`] px across when it is larger.
fn square(picture: &DynamicImage) -> DynamicImage {
    let side = picture.width().min(picture.height()).max(1);
    let left = (picture.width() - side.min(picture.width())) / 2;
    let top = (picture.height() - side.min(picture.height())) / 2;
    let square = picture.crop_imm(left, top, side, side);
    if side > SIDE {
        square.resize_exact(SIDE, SIDE, image::imageops::FilterType::Lanczos3)
    } else {
        square
    }
}

fn encode(picture: &DynamicImage) -> Result<Vec<u8>, PictureError> {
    let mut png = io::Cursor::new(Vec::new());
    picture
        .to_rgba8()
        .write_to(&mut png, image::ImageFormat::Png)
        .map_err(|error| PictureError::Unsaved(io::Error::other(error)))?;
    Ok(png.into_inner())
}

fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = teams_platform::private::replace(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(test)]
#[path = "../tests/support/account_picture_units.rs"]
mod tests;
