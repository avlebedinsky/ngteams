//! The devices the server holds, as the loop thread sees them: every node of a device's class (a microphone is an
//! `Audio/Source`, a speaker an `Audio/Sink`, a camera a `Video/Source`) from the registry, and the system's defaults
//! from the session manager's `default` metadata (`default.audio.source`, `default.audio.sink`,
//! `default.video.source`). Each change makes a new [`DeviceList`], kept for the devices' owner and told to its
//! watchers ([`Listing`]).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use pipewire::metadata::{Metadata, MetadataListener};
use pipewire::registry::{GlobalObject, Listener, RegistryRc};
use pipewire::spa::utils::dict::DictRef;
use pipewire::types::ObjectType;

use crate::devices::catalog::{DeviceInfo, DeviceKind, DeviceList, DeviceWatcher};

/// The metadata the session manager keeps the defaults in.
const DEFAULT_METADATA: &str = "default";

/// The list as last made, and who is told each new one; shared by the loop thread and the devices' owner.
#[derive(Default)]
pub(super) struct Listing {
    list: Mutex<DeviceList>,
    watchers: Mutex<Vec<DeviceWatcher>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Listing {
    pub(super) fn list(&self) -> DeviceList {
        lock(&self.list).clone()
    }

    pub(super) fn watch(&self, watcher: DeviceWatcher) {
        lock(&self.watchers).push(watcher);
    }

    /// Keeps `list` and tells the watchers, when it differs from the last.
    fn publish(&self, list: &DeviceList) {
        {
            let mut kept = lock(&self.list);
            if *kept == *list {
                return;
            }
            kept.clone_from(list);
        }
        for watcher in lock(&self.watchers).iter() {
            watcher(list);
        }
    }
}

/// The kind of device a node of `class` is, if it is one.
#[must_use]
pub(super) fn kind_of(class: &str) -> Option<DeviceKind> {
    match class {
        "Audio/Source" | "Audio/Source/Virtual" => Some(DeviceKind::Microphone),
        "Audio/Sink" => Some(DeviceKind::Speaker),
        "Video/Source" => Some(DeviceKind::Camera),
        _ => None,
    }
}

/// The kind a default's key names (`default.audio.source` and the others).
#[must_use]
pub(super) fn default_kind(key: &str) -> Option<DeviceKind> {
    match key {
        "default.audio.source" => Some(DeviceKind::Microphone),
        "default.audio.sink" => Some(DeviceKind::Speaker),
        "default.video.source" => Some(DeviceKind::Camera),
        _ => None,
    }
}

/// The node name a default's value holds (`{"name": "<node.name>"}`).
#[must_use]
pub(super) fn default_name(value: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(value).ok()?;
    value.get("name")?.as_str().map(str::to_owned)
}

/// A node's device, when it is one: its kind by class, its name, and what a person reads.
fn device_of(props: &DictRef) -> Option<DeviceInfo> {
    let kind = kind_of(props.get(*pipewire::keys::MEDIA_CLASS)?)?;
    let name = props.get(*pipewire::keys::NODE_NAME)?.to_owned();
    let description = props
        .get(*pipewire::keys::NODE_DESCRIPTION)
        .or_else(|| props.get(*pipewire::keys::NODE_NICK))
        .unwrap_or(&name)
        .to_owned();
    Some(DeviceInfo {
        kind,
        name,
        description,
    })
}

/// What the loop thread has seen.
#[derive(Default)]
struct Seen {
    nodes: HashMap<u32, DeviceInfo>,
    defaults: HashMap<DeviceKind, String>,
}

impl Seen {
    fn list(&self) -> DeviceList {
        let mut list = DeviceList::new(self.nodes.values().cloned().collect());
        for kind in DeviceKind::ALL {
            list = list.with_default(kind, self.defaults.get(&kind).map(String::as_str));
        }
        list
    }
}

/// The registry's and the default metadata's listeners, kept for the loop's life.
pub(super) struct Watching {
    _registry: Listener,
    _metadata: Rc<RefCell<Option<(MetadataListener, Metadata)>>>,
}

/// Starts watching `registry`: every change goes to `listing`.
pub(super) fn watch(registry: &RegistryRc, listing: Arc<Listing>) -> Watching {
    let seen = Rc::new(RefCell::new(Seen::default()));
    let metadata = Rc::new(RefCell::new(None));
    let weak = registry.downgrade();
    let on_global = {
        let (seen, metadata, listing) = (Rc::clone(&seen), Rc::clone(&metadata), Arc::clone(&listing));
        move |global: &GlobalObject<&DictRef>| {
            let Some(props) = global.props else {
                return;
            };
            match global.type_ {
                ObjectType::Node => {
                    if let Some(device) = device_of(props) {
                        seen.borrow_mut().nodes.insert(global.id, device);
                        listing.publish(&seen.borrow().list());
                    }
                }
                ObjectType::Metadata if props.get("metadata.name") == Some(DEFAULT_METADATA) => {
                    if let Some(registry) = weak.upgrade() {
                        *metadata.borrow_mut() = bind_defaults(&registry, global, &seen, &listing);
                    }
                }
                _ => {}
            }
        }
    };
    let on_remove = move |id| {
        let removed = seen.borrow_mut().nodes.remove(&id).is_some();
        if removed {
            listing.publish(&seen.borrow().list());
        }
    };
    let registry = registry
        .add_listener_local()
        .global(on_global)
        .global_remove(on_remove)
        .register();
    Watching {
        _registry: registry,
        _metadata: metadata,
    }
}

/// Binds the session manager's `default` metadata and keeps its defaults in `seen`.
fn bind_defaults(
    registry: &RegistryRc,
    global: &GlobalObject<&DictRef>,
    seen: &Rc<RefCell<Seen>>,
    listing: &Arc<Listing>,
) -> Option<(MetadataListener, Metadata)> {
    let metadata: Metadata = registry.bind(global).ok()?;
    let (seen, listing) = (Rc::clone(seen), Arc::clone(listing));
    let listener = metadata
        .add_listener_local()
        .property(move |subject, key, _type, value| {
            if subject == pipewire::core::PW_ID_CORE {
                take_default(&mut seen.borrow_mut(), key, value);
                listing.publish(&seen.borrow().list());
            }
            0
        })
        .register();
    Some((listener, metadata))
}

/// Takes one property of the `default` metadata: a default set or removed, or all of them removed (no key).
fn take_default(seen: &mut Seen, key: Option<&str>, value: Option<&str>) {
    let Some(key) = key else {
        seen.defaults.clear();
        return;
    };
    let Some(kind) = default_kind(key) else {
        return;
    };
    match value.and_then(default_name) {
        Some(name) => {
            seen.defaults.insert(kind, name);
        }
        None => {
            seen.defaults.remove(&kind);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_classes_and_default_keys_are_read_as_the_session_manager_writes_them() {
        assert_eq!(kind_of("Audio/Source"), Some(DeviceKind::Microphone));
        assert_eq!(kind_of("Audio/Source/Virtual"), Some(DeviceKind::Microphone));
        assert_eq!(kind_of("Audio/Sink"), Some(DeviceKind::Speaker));
        assert_eq!(kind_of("Video/Source"), Some(DeviceKind::Camera));
        assert_eq!(kind_of("Stream/Input/Audio"), None, "a stream is no device");
        assert_eq!(default_kind("default.audio.sink"), Some(DeviceKind::Speaker));
        assert_eq!(default_kind("default.configured.audio.sink"), None);
        assert_eq!(
            default_name(r#"{ "name": "sandbox-speaker" }"#).as_deref(),
            Some("sandbox-speaker")
        );
        assert_eq!(default_name("sandbox-speaker"), None);
    }

    #[test]
    fn defaults_come_and_go() {
        let mut seen = Seen::default();
        take_default(&mut seen, Some("default.audio.source"), Some(r#"{"name":"mic"}"#));
        take_default(&mut seen, Some("default.video.source"), Some(r#"{"name":"cam"}"#));
        take_default(&mut seen, Some("default.audio.source.other"), Some(r#"{"name":"x"}"#));
        assert_eq!(seen.list().default_name(DeviceKind::Microphone), Some("mic"));
        take_default(&mut seen, Some("default.audio.source"), None);
        assert_eq!(seen.list().default_name(DeviceKind::Microphone), None);
        assert_eq!(seen.list().default_name(DeviceKind::Camera), Some("cam"));
        take_default(&mut seen, None, None);
        assert_eq!(seen.list().default_name(DeviceKind::Camera), None);
    }

    #[test]
    fn a_list_is_told_only_when_it_changed() {
        let listing = Listing::default();
        let told = Arc::new(Mutex::new(0));
        let count = Arc::clone(&told);
        listing.watch(Box::new(move |_| *lock(&count) += 1));
        listing.publish(&DeviceList::default());
        assert_eq!(*lock(&told), 0);
        let list = DeviceList::default().with_default(DeviceKind::Speaker, Some("spk"));
        listing.publish(&list);
        listing.publish(&list);
        assert_eq!(*lock(&told), 1);
        assert_eq!(listing.list(), list);
    }
}
