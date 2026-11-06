//! The desktop's camera portal (`org.freedesktop.portal.Camera`): how the personal build reaches a camera, as a
//! sandboxed desktop app does. The portal asks the user (or remembers their answer) and then hands over a PipeWire
//! remote that shows only the cameras; the camera's stream runs on that remote.
//!
//! The desktop's screen-cast portal (`org.freedesktop.portal.ScreenCast`, [`open_screen_cast`]) is how a call shares a
//! screen: `CreateSession`, `SelectSources` (a monitor or a window, the cursor drawn in), `Start` (the portal asks the
//! user which, and answers with the PipeWire node of what they picked), then `OpenPipeWireRemote` on that session. The
//! session lives as long as the [`ScreenCast`] (closed when it goes); the screen's stream runs on its remote.
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
/// The screen cast's interface, and its sessions'.
pub const SCREEN_CAST_INTERFACE: &str = "org.freedesktop.portal.ScreenCast";
pub const SESSION_INTERFACE: &str = "org.freedesktop.portal.Session";
/// `SelectSources`' types: a monitor or a window (`1 | 2`); its cursor mode: drawn into the picture (`2`).
const MONITOR_OR_WINDOW: u32 = 1 | 2;
const CURSOR_EMBEDDED: u32 = 2;
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
    let (code, _) = request(connection, "teams_camera", |options| {
        camera.call("AccessCamera", &(options,)).map_err(portal)
    })?;
    access_granted(code)
}

/// A portal request: `call` makes it with the options naming its `handle_token` (more options added to them), and its
/// answer comes as the `Response` signal of the request object that token names (listened to before the call, at the
/// path the portal derives from our unique name and the token): its code and its results.
fn request(
    connection: &Connection,
    prefix: &str,
    call: impl FnOnce(HashMap<&str, Value<'_>>) -> Result<OwnedObjectPath, DeviceError>,
) -> Result<(u32, HashMap<String, OwnedValue>), DeviceError> {
    let token = token(prefix);
    let unique = connection
        .unique_name()
        .ok_or_else(|| portal("no unique name on the bus"))?;
    let path = request_path(unique.as_str(), &token);
    let request = Proxy::new(connection, PORTAL_NAME, path.as_str(), REQUEST_INTERFACE).map_err(portal)?;
    let mut responses = request.receive_signal("Response").map_err(portal)?;
    let mut options = HashMap::new();
    options.insert("handle_token", Value::from(token.as_str()));
    let handle = call(options)?;
    if handle.as_str() != path {
        return Err(portal(format!("the answer would come at {handle}, not at {path}")));
    }
    let (answered, answer) = mpsc::channel();
    thread::Builder::new()
        .name("teams-media-portal".to_owned())
        .spawn(move || {
            let answer = responses.next().map(|message| {
                message
                    .body()
                    .deserialize::<(u32, HashMap<String, OwnedValue>)>()
                    .map_err(portal)
            });
            let _ = answered.send(answer);
        })
        .map_err(DeviceError::Io)?;
    match answer.recv_timeout(ANSWER_WITHIN) {
        Ok(Some(answer)) => answer,
        Ok(None) => Err(portal("the bus closed before the portal answered")),
        Err(_) => {
            // the waiting thread ends with the connection
            let _ = connection.clone().close();
            Err(portal(format!("no answer within {ANSWER_WITHIN:?}")))
        }
    }
}

/// A fresh token of this process's requests and sessions, starting `prefix`.
fn token(prefix: &str) -> String {
    format!(
        "{prefix}_{}_{}",
        std::process::id(),
        NEXT_TOKEN.fetch_add(1, Ordering::Relaxed)
    )
}

/// What the screen-cast portal's answer to a request means: its results when the user went on (0).
///
/// # Errors
/// [`DeviceError::Refused`] when the user cancelled (1) or the portal ended the request (2 or another code).
fn screen_cast_answered(
    (code, results): (u32, HashMap<String, OwnedValue>),
) -> Result<HashMap<String, OwnedValue>, DeviceError> {
    match code {
        0 => Ok(results),
        1 => Err(DeviceError::Refused("no screen was picked to share".to_owned())),
        other => Err(DeviceError::Refused(format!(
            "the screen-cast portal ended the request (answer {other})"
        ))),
    }
}

/// A screen the user picked to share at the screen-cast portal: its session (closed when this goes), the PipeWire node
/// that carries its picture, and the remote to reach it on.
pub struct ScreenCast {
    connection: Connection,
    session: OwnedObjectPath,
    /// The node of what the user picked, on [`ScreenCast::remote`].
    pub node: u32,
    /// The PipeWire remote the portal handed over; taken once.
    pub remote: Option<OwnedFd>,
}

impl std::fmt::Debug for ScreenCast {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScreenCast")
            .field("node", &self.node)
            .finish_non_exhaustive()
    }
}

impl Drop for ScreenCast {
    fn drop(&mut self) {
        // the portal ends the cast with its session (and with our connection, the next moment)
        if let Ok(session) = Proxy::new(&self.connection, PORTAL_NAME, self.session.as_str(), SESSION_INTERFACE) {
            let _: Result<(), _> = session.call("Close", &());
        }
    }
}

/// Asks the screen-cast portal on the bus at `address` for a screen or a window to share (the user picks it there), and
/// returns its session, node and remote.
///
/// # Errors
/// [`DeviceError::Refused`] when the user picked nothing or the portal ended a request, [`DeviceError::Portal`] when
/// the bus or the portal fails, answers in a shape it doesn't document, or doesn't answer within [`ANSWER_WITHIN`].
pub fn open_screen_cast(address: &str) -> Result<ScreenCast, DeviceError> {
    let connection = Builder::address(address).map_err(portal)?.build().map_err(portal)?;
    let cast = Proxy::new(&connection, PORTAL_NAME, PORTAL_PATH, SCREEN_CAST_INTERFACE).map_err(portal)?;
    let session_token = token("teams_screen");
    let created = screen_cast_answered(request(&connection, "teams_screen", |mut options| {
        options.insert("session_handle_token", Value::from(session_token.clone()));
        cast.call("CreateSession", &(options,)).map_err(portal)
    })?)?;
    let session = created
        .get("session_handle")
        .and_then(|handle| String::try_from(handle.try_clone().ok()?).ok())
        .and_then(|handle| OwnedObjectPath::try_from(handle).ok())
        .ok_or_else(|| portal("CreateSession's answer names no session"))?;
    let mut opened = ScreenCast {
        connection,
        session,
        node: 0,
        remote: None,
    };
    screen_cast_answered(request(&opened.connection, "teams_screen", |mut options| {
        options.insert("types", Value::from(MONITOR_OR_WINDOW));
        options.insert("multiple", Value::from(false));
        options.insert("cursor_mode", Value::from(CURSOR_EMBEDDED));
        cast.call("SelectSources", &(&opened.session, options)).map_err(portal)
    })?)?;
    let started = screen_cast_answered(request(&opened.connection, "teams_screen", |options| {
        cast.call("Start", &(&opened.session, "", options)).map_err(portal)
    })?)?;
    opened.node = first_stream(&started).ok_or_else(|| portal("Start's answer names no stream"))?;
    let options: HashMap<&str, Value<'_>> = HashMap::new();
    let remote: zbus::zvariant::OwnedFd = cast
        .call("OpenPipeWireRemote", &(&opened.session, options))
        .map_err(portal)?;
    opened.remote = Some(remote.into());
    Ok(opened)
}

/// The node of the first stream `Start`'s results name (`streams`: `a(ua{sv})`).
fn first_stream(results: &HashMap<String, OwnedValue>) -> Option<u32> {
    let streams = results.get("streams")?.try_clone().ok()?;
    let streams = <Vec<(u32, HashMap<String, OwnedValue>)>>::try_from(streams).ok()?;
    streams.first().map(|(node, _)| *node)
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
    fn only_an_answer_of_zero_goes_on_to_the_screen() {
        assert!(screen_cast_answered((0, HashMap::new())).is_ok());
        for code in [1, 2, 3] {
            assert!(
                matches!(
                    screen_cast_answered((code, HashMap::new())),
                    Err(DeviceError::Refused(_))
                ),
                "{code}"
            );
        }
    }

    #[test]
    fn the_first_stream_of_start_s_answer_names_the_node() {
        let streams: Vec<(u32, HashMap<String, OwnedValue>)> = vec![(42, HashMap::new()), (7, HashMap::new())];
        let mut results = HashMap::new();
        results.insert(
            "streams".to_owned(),
            OwnedValue::try_from(Value::from(streams)).unwrap(),
        );
        assert_eq!(first_stream(&results), Some(42));
        assert_eq!(first_stream(&HashMap::new()), None);
    }

    #[test]
    fn only_an_answer_of_zero_allows_the_camera() {
        assert!(access_granted(0).is_ok());
        for code in [1, 2, 3] {
            assert!(matches!(access_granted(code), Err(DeviceError::Refused(_))), "{code}");
        }
    }
}
