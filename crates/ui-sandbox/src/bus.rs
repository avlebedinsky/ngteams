//! The sandbox's private D-Bus session bus and the fakes on it.
//!
//! `dbus-daemon` runs on a config of our own rather than `--session`: the stock session config activates services
//! from the desktop's service dirs (a portal, a notification daemon, a keyring), which would start them on this bus.
//! Ours lists no service dir, so a name nobody owns stays unowned and a call to it fails at once
//! (`ServiceUnknown`); its socket lies in the client's runtime dir as `bus`, where a client that falls back from a
//! missing `DBUS_SESSION_BUS_ADDRESS` to `$XDG_RUNTIME_DIR/bus` finds it too.
//!
//! On it, from this process (each over a connection to this bus's address, never one taken from the environment):
//! a fake notification server (`org.freedesktop.Notifications`) and a fake `StatusNotifierItem` watcher
//! (`org.kde.StatusNotifierWatcher`), each recording every call with its sender.

use std::{
    collections::HashMap,
    fmt::Write as _,
    fs,
    path::Path,
    process::{Child, Stdio},
    sync::{Arc, Mutex, PoisonError},
};

use zbus::{
    blocking::{Connection, connection::Builder, fdo::DBusProxy},
    interface,
    message::Header,
    names::BusName,
    zvariant::Value,
};

use crate::{SOCKET_WAIT, SandboxResult, clean_command, spawn, wait_until};

/// The daemon's log's name in the sandbox dir.
pub const LOG: &str = "dbus.log";

/// The notification server's bus name and object path.
pub const NOTIFICATIONS_NAME: &str = "org.freedesktop.Notifications";
pub const NOTIFICATIONS_PATH: &str = "/org/freedesktop/Notifications";
/// The `StatusNotifierItem` watcher's bus name and object path.
pub const WATCHER_NAME: &str = "org.kde.StatusNotifierWatcher";
pub const WATCHER_PATH: &str = "/StatusNotifierWatcher";

/// One `Notify` call the fake server got.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
    /// The caller's unique bus name.
    pub sender: String,
    /// The id the server answered with.
    pub id: u32,
    pub app_name: String,
    pub replaces_id: u32,
    pub app_icon: String,
    pub summary: String,
    pub body: String,
    pub actions: Vec<String>,
    /// The hints' keys, sorted (their values aren't kept).
    pub hints: Vec<String>,
    pub expire_timeout: i32,
}

/// One `RegisterStatusNotifierItem` call the fake watcher got.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayItem {
    /// The caller's unique bus name.
    pub sender: String,
    /// The service it named: a bus name, or an object path on the caller's connection.
    pub service: String,
}

/// What the fakes recorded, shared with them.
#[derive(Default)]
struct Records {
    notifications: Vec<Notification>,
    closed: Vec<u32>,
    tray_items: Vec<TrayItem>,
    tray_hosts: Vec<String>,
}

type Shared = Arc<Mutex<Records>>;

fn records(shared: &Shared) -> std::sync::MutexGuard<'_, Records> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

fn sender(header: &Header<'_>) -> String {
    header.sender().map(ToString::to_string).unwrap_or_default()
}

/// The fake `org.freedesktop.Notifications`.
struct Notifications {
    records: Shared,
    next_id: u32,
}

/// What a `Notify` call carries apart from its sender.
struct Notify<'a> {
    app_name: &'a str,
    replaces_id: u32,
    app_icon: &'a str,
    summary: &'a str,
    body: &'a str,
    actions: Vec<&'a str>,
    hints: Vec<String>,
    expire_timeout: i32,
}

impl Notifications {
    fn record(&mut self, sender: String, call: Notify<'_>) -> u32 {
        let id = if call.replaces_id == 0 {
            self.next_id = self.next_id.wrapping_add(1).max(1);
            self.next_id
        } else {
            call.replaces_id
        };
        let mut hints = call.hints;
        hints.sort();
        records(&self.records).notifications.push(Notification {
            sender,
            id,
            app_name: call.app_name.to_owned(),
            replaces_id: call.replaces_id,
            app_icon: call.app_icon.to_owned(),
            summary: call.summary.to_owned(),
            body: call.body.to_owned(),
            actions: call.actions.iter().map(|&action| action.to_owned()).collect(),
            hints,
            expire_timeout: call.expire_timeout,
        });
        id
    }
}

#[interface(name = "org.freedesktop.Notifications")]
impl Notifications {
    #[expect(
        clippy::too_many_arguments,
        reason = "the D-Bus method's own signature (susssasa{sv}i), which zbus maps to one argument each"
    )]
    #[expect(
        clippy::needless_pass_by_value,
        reason = "zbus hands a method its message header by value"
    )]
    fn notify(
        &mut self,
        #[zbus(header)] header: Header<'_>,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: Vec<&str>,
        hints: HashMap<&str, Value<'_>>,
        expire_timeout: i32,
    ) -> u32 {
        let call = Notify {
            app_name,
            replaces_id,
            app_icon,
            summary,
            body,
            actions,
            hints: hints.into_keys().map(str::to_owned).collect(),
            expire_timeout,
        };
        self.record(sender(&header), call)
    }

    fn close_notification(&self, id: u32) {
        records(&self.records).closed.push(id);
    }

    #[expect(clippy::unused_self, reason = "a D-Bus method zbus calls on the object")]
    fn get_capabilities(&self) -> Vec<&'static str> {
        vec!["body", "actions"]
    }

    #[expect(clippy::unused_self, reason = "a D-Bus method zbus calls on the object")]
    fn get_server_information(&self) -> (&'static str, &'static str, &'static str, &'static str) {
        ("ui-sandbox", "ms-teams", "0", "1.2")
    }
}

/// The fake `org.kde.StatusNotifierWatcher`: it says a host is there, so an app registers its item.
struct Watcher {
    records: Shared,
}

#[interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    #[expect(
        clippy::needless_pass_by_value,
        reason = "zbus hands a method its message header by value"
    )]
    fn register_status_notifier_item(&self, #[zbus(header)] header: Header<'_>, service: &str) {
        records(&self.records).tray_items.push(TrayItem {
            sender: sender(&header),
            service: service.to_owned(),
        });
    }

    fn register_status_notifier_host(&self, service: &str) {
        records(&self.records).tray_hosts.push(service.to_owned());
    }

    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        records(&self.records)
            .tray_items
            .iter()
            .map(|item| item.service.clone())
            .collect()
    }

    #[zbus(property)]
    #[expect(clippy::unused_self, reason = "a D-Bus property zbus reads on the object")]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }

    #[zbus(property)]
    #[expect(clippy::unused_self, reason = "a D-Bus property zbus reads on the object")]
    fn protocol_version(&self) -> i32 {
        0
    }
}

/// `text` as a D-Bus address value: bytes outside `[-0-9A-Za-z_/.\*]` percent-escaped.
#[must_use]
pub fn escape_address_value(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_/.\\*".contains(&byte) {
            escaped.push(char::from(byte));
        } else {
            let _ = write!(escaped, "%{byte:02x}");
        }
    }
    escaped
}

fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The daemon's config: a session bus on `address`, no service activation, everyone may own and call anything.
fn config(address: &str) -> String {
    format!(
        "<!DOCTYPE busconfig PUBLIC \"-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN\"\n \
         \"http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd\">\n\
         <busconfig>\n  <type>session</type>\n  <keep_umask/>\n  <listen>{}</listen>\n  <auth>EXTERNAL</auth>\n  \
         <policy context=\"default\">\n    <allow send_destination=\"*\" eavesdrop=\"true\"/>\n    \
         <allow eavesdrop=\"true\"/>\n    <allow own=\"*\"/>\n  </policy>\n</busconfig>\n",
        escape_xml(address)
    )
}

/// A private `dbus-daemon` and the fakes on it; the daemon is killed on drop.
pub struct PrivateBus {
    daemon: Child,
    address: String,
    records: Shared,
    /// The fakes' connections, kept open for the bus's life.
    connections: Vec<Connection>,
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

impl PrivateBus {
    /// Starts `dbus-daemon` listening on `socket`, with its config and log in `dir`, then the fakes on it.
    ///
    /// # Errors
    /// The daemon fails to start or make its socket in time, or a fake can't take its name.
    pub fn start(dbus_daemon: &Path, socket: &Path, dir: &Path) -> SandboxResult<Self> {
        let address = format!("unix:path={}", escape_address_value(&socket.to_string_lossy()));
        let config_file = dir.join("dbus.conf");
        fs::write(&config_file, config(&address))?;
        let mut command = clean_command(dbus_daemon);
        command
            .arg(format!("--config-file={}", config_file.display()))
            .args(["--nofork", "--nopidfile"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(dir.join(LOG))?);
        spawn::pass_only(&mut command, &[]);
        let daemon = command.spawn()?;
        let mut bus = Self {
            daemon,
            address,
            records: Shared::default(),
            connections: Vec::new(),
        };
        if wait_until(SOCKET_WAIT, || socket.exists().then_some(())).is_none() {
            let log = fs::read_to_string(dir.join(LOG)).unwrap_or_default();
            return Err(format!("dbus-daemon made no socket at {}: {log}", socket.display()).into());
        }
        bus.serve()?;
        Ok(bus)
    }

    fn serve(&mut self) -> SandboxResult {
        let notifications = Notifications {
            records: Arc::clone(&self.records),
            next_id: 0,
        };
        let notifications = Builder::address(self.address.as_str())?
            .name(NOTIFICATIONS_NAME)?
            .serve_at(NOTIFICATIONS_PATH, notifications)?
            .build()?;
        let watcher = Watcher {
            records: Arc::clone(&self.records),
        };
        let watcher = Builder::address(self.address.as_str())?
            .name(WATCHER_NAME)?
            .serve_at(WATCHER_PATH, watcher)?
            .build()?;
        self.connections.extend([notifications, watcher]);
        Ok(())
    }

    /// The bus's address, for `DBUS_SESSION_BUS_ADDRESS`.
    #[must_use]
    pub fn address(&self) -> &str {
        &self.address
    }

    /// The daemon's process id.
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.daemon.id()
    }

    /// A new connection to this bus (its address only, never the environment's).
    ///
    /// # Errors
    /// The bus refuses it.
    pub fn connect(&self) -> SandboxResult<Connection> {
        Ok(Builder::address(self.address.as_str())?.build()?)
    }

    /// The `Notify` calls the fake server got, in order.
    #[must_use]
    pub fn notifications(&self) -> Vec<Notification> {
        records(&self.records).notifications.clone()
    }

    /// The ids `CloseNotification` was called with, in order.
    #[must_use]
    pub fn closed_notifications(&self) -> Vec<u32> {
        records(&self.records).closed.clone()
    }

    /// The items registered with the fake watcher, in order.
    #[must_use]
    pub fn tray_items(&self) -> Vec<TrayItem> {
        records(&self.records).tray_items.clone()
    }

    /// The hosts registered with the fake watcher, in order.
    #[must_use]
    pub fn tray_hosts(&self) -> Vec<String> {
        records(&self.records).tray_hosts.clone()
    }

    /// The process ids of every connection on the bus but the daemon's own (the fakes' included).
    ///
    /// # Errors
    /// The bus can't be asked.
    pub fn client_pids(&self) -> SandboxResult<Vec<u32>> {
        let connection = self.connections.first().ok_or("no connection to the bus")?;
        let proxy = DBusProxy::new(connection)?;
        let mut pids = Vec::new();
        for name in proxy.list_names()? {
            if name.starts_with(':') {
                pids.push(proxy.get_connection_unix_process_id(BusName::from(name))?);
            }
        }
        pids.sort_unstable();
        pids.dedup();
        Ok(pids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_value_escapes_what_dbus_reserves() {
        assert_eq!(escape_address_value("/tmp/.tmpAb_c-1/bus"), "/tmp/.tmpAb_c-1/bus");
        assert_eq!(escape_address_value("/tmp/a b,c=d;e"), "/tmp/a%20b%2cc%3dd%3be");
    }

    #[test]
    fn the_config_activates_nothing() {
        let config = config("unix:path=/tmp/x&y/bus");
        assert!(
            config.contains("<listen>unix:path=/tmp/x&amp;y/bus</listen>"),
            "{config}"
        );
        for activation in ["servicedir", "standard_session_servicedirs", "include"] {
            assert!(!config.contains(activation), "{activation} in {config}");
        }
    }
}
