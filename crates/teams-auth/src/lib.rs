//! Native Teams sign-in: the PKCE code exchange, token refresh and storage, and the skypetoken. The interactive step
//! always runs in a browser engine; this crate only handles the tokens.
//!
//! - [`pkce`]: the PKCE verifier and challenge, and the OAuth `state`;
//! - [`token`]: the authorize URL, the scope rule, and the grants against Entra's token endpoint;
//! - [`id_token`]: the account an ID token names, checked before a code exchange stores anything;
//! - [`refresh`]: the code exchange and refresh with the refresh token kept in a [`secrets::SecretStore`];
//! - [`secrets`]: the store itself.
//!
//! Filled in by M0 Tasks 2, 13 and 14 (`docs/plans/20261019-native-teams-m0-lab-and-spikes.md`).

pub mod id_token;
pub mod pkce;
pub mod refresh;
pub mod secrets;
pub mod token;
