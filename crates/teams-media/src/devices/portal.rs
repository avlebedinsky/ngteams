//! The desktop's camera portal (`org.freedesktop.portal.Camera`): how the personal build reaches a camera, as a
//! sandboxed desktop app does. The portal asks the user (or remembers their answer) and then hands over a PipeWire
//! remote that shows only the cameras; the camera's stream runs on that remote.
//!
//! [`open_camera_remote`] makes the two calls on the bus it is given, each on a connection of its own to that address
//! (never one taken from the environment): `AccessCamera`, whose answer comes as the `Response` signal of the request
//! object the call names (listened to before the call, at the path the portal derives from our unique name and our
//! token), then `OpenPipeWireRemote`, which answers with the remote's descriptor. Which bus a reach may ask is the
//! PipeWire backend's to decide ([`super::pipewire::CameraRoute`]): sandbox reach asks only the UI sandbox's, where
//! the sandbox's fake portal answers.

use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use zbus::blocking::connection::Builder;
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use super::DeviceError;

/// The portal's bus name and object.
pub const PORTAL_NAME: &str = "org.freedesktop.portal.Desktop";
pub const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
/// The camera's interface, and the interface of the request object its answer comes on.
pub const CAMERA_INTERFACE: &str = "org.freedesktop.portal.Camera";
pub const REQUEST_INTERFACE: &str = "org.freedesktop.portal.Request";
/// How long the user gets to answer the portal's question.
pub const ANSWER_WITHIN: Duration = Duration::from_secs(120);

/// Tokens of this process's requests.
static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

/// The request object's path for the caller with `unique_name` and the `token` it chose (the portal's own rule: the
/// unique name without its `:`, its dots turned into underscores).
#[must_use]
pub fn request_path(unique_name: &str, token: &str) -> String {
    let sender = unique_name.trim_start_matches(':').replace('.', "_");
    format!("{PORTAL_PATH}/request/{sender}/{token}")
}

/// What the portal's answer to `AccessCamera` means.
///
/// # Errors
/// [`DeviceError::Refused`] when the user didn't allow it (1) or the portal ended the request (2 or another code).
pub fn access_granted(code: u32) -> Result<(), DeviceError> {
    match code {
        0 => Ok(()),
        1 => Err(DeviceError::Refused(
            "the camera portal: the camera wasn't allowed".to_owned(),
        )),
        other => Err(DeviceError::Refused(format!(
            "the camera portal ended the request (answer {other})"
        ))),
    }
}

fn portal(error: impl std::fmt::Display) -> DeviceError {
    DeviceError::Portal(error.to_string())
}

/// Asks the camera portal on the bus at `address` for the camera, and returns the PipeWire remote it hands over.
///
/// # Errors
/// [`DeviceError::Refused`] when the portal says there is no camera or the user doesn't allow it,
/// [`DeviceError::Portal`] when the bus or the portal fails or doesn't answer within [`ANSWER_WITHIN`].
pub fn open_camera_remote(address: &str) -> Result<OwnedFd, DeviceError> {
    let connection = Builder::address(address).map_err(portal)?.build().map_err(portal)?;
    let camera = Proxy::new(&connection, PORTAL_NAME, PORTAL_PATH, CAMERA_INTERFACE).map_err(portal)?;
    let present: bool = camera.get_property("IsCameraPresent").map_err(portal)?;
    if !present {
        return Err(DeviceError::Refused(
            "the camera portal says there is no camera".to_owned(),
        ));
    }
    access(&connection, &camera)?;
    let options: HashMap<&str, Value<'_>> = HashMap::new();
    let remote: zbus::zvariant::OwnedFd = camera.call("OpenPipeWireRemote", &(options,)).map_err(portal)?;
    Ok(remote.into())
}

/// `AccessCamera`, and its answer.
fn access(connection: &Connection, camera: &Proxy<'_>) -> Result<(), DeviceError> {
    let token = format!(
        "teams_camera_{}_{}",
        std::process::id(),
        NEXT_TOKEN.fetch_add(1, Ordering::Relaxed)
    );
    let unique = connection
        .unique_name()
        .ok_or_else(|| portal("no unique name on the bus"))?;
    let path = request_path(unique.as_str(), &token);
    let request = Proxy::new(connection, PORTAL_NAME, path.as_str(), REQUEST_INTERFACE).map_err(portal)?;
    let mut responses = request.receive_signal("Response").map_err(portal)?;
    let mut options = HashMap::new();
    options.insert("handle_token", Value::from(token.as_str()));
    let handle: OwnedObjectPath = camera.call("AccessCamera", &(options,)).map_err(portal)?;
    if handle.as_str() != path {
        return Err(portal(format!("the answer would come at {handle}, not at {path}")));
    }
    let (answered, answer) = mpsc::channel();
    thread::Builder::new()
        .name("teams-media-portal".to_owned())
        .spawn(move || {
            let code = responses.next().map(|message| {
                message
                    .body()
                    .deserialize::<(u32, HashMap<String, OwnedValue>)>()
                    .map(|(code, _)| code)
                    .map_err(portal)
            });
            let _ = answered.send(code);
        })
        .map_err(DeviceError::Io)?;
    match answer.recv_timeout(ANSWER_WITHIN) {
        Ok(Some(code)) => access_granted(code?),
        Ok(None) => Err(portal("the bus closed before the portal answered")),
        Err(_) => {
            // the waiting thread ends with the connection
            let _ = connection.clone().close();
            Err(portal(format!("no answer within {ANSWER_WITHIN:?}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_request_path_is_the_portal_s_own() {
        assert_eq!(
            request_path(":1.42", "teams_camera_7_1"),
            "/org/freedesktop/portal/desktop/request/1_42/teams_camera_7_1"
        );
    }

    #[test]
    fn only_an_answer_of_zero_allows_the_camera() {
        assert!(access_granted(0).is_ok());
        for code in [1, 2, 3] {
            assert!(matches!(access_granted(code), Err(DeviceError::Refused(_))), "{code}");
        }
    }
}
