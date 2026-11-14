//! The first-admin setup code: while the server has no admin, whoever holds the code may create the first one
//! (`POST /api/admin/v1/setup`). A node that starts without an admin issues a code and logs it once; the command
//! line (`ngteams-server setup-code`) issues a new one, which replaces it. Only the code's SHA-256 is kept, in the
//! database, so any node checks it; creating the first admin spends it.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest as _, Sha256};

use super::{AdminError, repo};
use crate::app::App;
use crate::db::Db;

/// Crockford's base32 digits: no `I`, `L`, `O` or `U`.
const ALPHABET: [char; 32] = [
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'J', 'K', 'M', 'N', 'P',
    'Q', 'R', 'S', 'T', 'V', 'W', 'X', 'Y', 'Z',
];
/// A code is five groups of five digits (125 random bits).
const GROUP: usize = 5;
const DIGITS: usize = 5 * GROUP;

/// A fresh code as a person reads it (`XXXXX-XXXXX-XXXXX-XXXXX-XXXXX`).
fn mint() -> Result<String, AdminError> {
    let mut bytes = [0_u8; DIGITS];
    aws_lc_rs::rand::fill(&mut bytes).map_err(|_| AdminError::Random)?;
    let mut code = String::with_capacity(DIGITS + DIGITS / GROUP);
    for (index, byte) in bytes.iter().enumerate() {
        if index > 0 && index % GROUP == 0 {
            code.push('-');
        }
        // 256 is a multiple of 32, so each digit is as likely as any other
        code.extend(ALPHABET.get(usize::from(byte % 32)));
    }
    Ok(code)
}

/// How a code is kept and compared: the SHA-256 of its digits, however they were typed (any case, with or without
/// the dashes, `O` for `0`, `I` or `L` for `1`).
#[must_use]
pub fn digest(code: &str) -> String {
    let digits: String = code
        .chars()
        .filter(|character| !character.is_whitespace() && *character != '-')
        .map(|character| match character.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            other => other,
        })
        .collect();
    URL_SAFE_NO_PAD.encode(Sha256::digest(digits.as_bytes()))
}

/// Issues a new setup code, replacing a pending one (the command line).
///
/// # Errors
/// [`AdminError::AdminExists`] when the server has an admin; a database failure.
pub async fn issue(db: &Db) -> Result<String, AdminError> {
    let code = mint()?;
    repo::replace_setup_code(db, &digest(&code)).await?;
    Ok(code)
}

/// What a starting node says about the setup.
#[derive(Debug, PartialEq, Eq)]
enum Pending {
    /// An admin exists.
    None,
    /// This node issued the code.
    Issued(String),
    /// A code was issued before (another node, or an earlier start).
    Earlier,
    /// The server asks for no code.
    Open,
}

async fn pending(app: &App) -> Result<Pending, AdminError> {
    if repo::admins(&app.db).await? > 0 {
        return Ok(Pending::None);
    }
    if !app.config.setup.require_code {
        return Ok(Pending::Open);
    }
    let code = mint()?;
    Ok(if repo::offer_setup_code(&app.db, &digest(&code)).await? {
        Pending::Issued(code)
    } else {
        Pending::Earlier
    })
}

/// On a node's start: without an admin, issues the setup code unless one is pending, and logs it (once).
pub async fn announce(app: &App) {
    let message = match pending(app).await {
        Ok(Pending::None) => return,
        Ok(Pending::Issued(code)) => {
            format!("no admin account yet: create the first one from ngteams with this setup code: {code}")
        }
        Ok(Pending::Earlier) => {
            "no admin account yet: a setup code was issued earlier; `ngteams-server setup-code` issues a new one"
                .to_owned()
        }
        Ok(Pending::Open) => {
            "no admin account yet, and setup.require_code is off: whoever reaches this server first creates the admin"
                .to_owned()
        }
        Err(error) => format!("can't prepare the first-admin setup: {error}"),
    };
    tracing::warn!("{message}");
}
