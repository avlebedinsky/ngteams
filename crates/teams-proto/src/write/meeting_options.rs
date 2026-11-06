//! A call's meeting options, as its organizer's Controls read and change them while it runs (seen 2026-11-06, `ab-call`:
//! a 1:1 call escalated to a group call, its Controls; `docs/protocol/calling.md`, "More of a running call"). They live
//! on the middle tier at `beta/meetings/v1/options/<tenant>/<organizer object id>/<chat>/0/0?monarch=true`: a `GET`
//! lists every option with its values and its current value ([`MeetingOptionsRead`]), and a `POST` of
//! `{"options": [{"name", "type", "currentValue"}]}` changes one ([`SetMeetingOption`]), answered `200` with the option
//! as it now is. Everyone in the call learns a change from the conversation's next update,
//! `meetingDetails.meetingCapability` ([`MeetingOptions::of_capability`]).
//!
//! Controls' rows and the options they change:
//! - Enable lobby: `AutoAdmittedUsers` (a list): `EveryoneInCompany` on, `Everyone` off.
//! - Present: `PresenterOption` (a list): `Everyone` on, `OrganizerOnly` off.
//! - Chat: `AllowMeetingChat` (a list): `Enabled` on, `Disabled` off. Teams' own menu sent `Disabled` turning it back on;
//!   `Enabled` is the option's value its read and its answers list for "On".
//! - Unmute themselves and Turn on their cameras: `AttendeeRestrictions` (a bit mask): `UnRestricted`, `DisableAudio`,
//!   `DisableVideo` or `DisableAudio,DisableVideo` (told as `DisableAv` in the conversation's update).

use serde_json::{Value, json};

use super::{Prepared, Wire};
use crate::endpoint::{Endpoint, Method, Service};
use crate::read::{Read, is_guid};

/// Whose meeting options: the tenant, the organizer's object id and the call's chat (a group chat or a meeting's).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct OptionsOf {
    tenant: String,
    organizer: String,
    chat: String,
}

impl OptionsOf {
    /// The options of the call in `chat` the account `organizer` (an object id) organizes in `tenant`; `None` unless
    /// both ids are GUIDs and `chat` a group chat's or a meeting chat's id.
    #[must_use]
    pub fn new(tenant: &str, organizer: &str, chat: &str) -> Option<Self> {
        let chat_ok = crate::is_group_chat_id(chat) || crate::is_meeting_chat_id(chat);
        (is_guid(tenant) && is_guid(organizer) && chat_ok).then(|| Self {
            tenant: tenant.to_ascii_lowercase(),
            organizer: organizer.to_ascii_lowercase(),
            chat: chat.to_owned(),
        })
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new([
            "beta",
            "meetings",
            "v1",
            "options",
            self.tenant.as_str(),
            self.organizer.as_str(),
            self.chat.as_str(),
            "0",
            "0",
        ])
        .with_query("monarch", "true")
    }
}

/// What Controls shows of the options, each where it was told (`None`: not known).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MeetingOptions {
    /// Enable lobby: anyone outside the organization waits in the lobby (`AutoAdmittedUsers` isn't `Everyone`).
    pub lobby: Option<bool>,
    /// Allow all participants to present (`PresenterOption` `Everyone` or `EveryoneInCompany`): everyone is a
    /// presenter then, and the restrictions below bind nobody.
    pub present: Option<bool>,
    /// Allow all participants to chat (`AllowMeetingChat` `Enabled` or `InMeeting`).
    pub chat: Option<bool>,
    /// Allow attendees to unmute themselves (`AttendeeRestrictions` without `DisableAudio`).
    pub unmute: Option<bool>,
    /// Allow attendees to turn on their cameras (`AttendeeRestrictions` without `DisableVideo`).
    pub camera: Option<bool>,
}

impl MeetingOptions {
    /// The options an options read's or change's answer `body` lists (`{"options": [{"name", "currentValue"}]}`); `None`
    /// unless it is an object with an `options` array.
    #[must_use]
    pub fn read_answer(body: &[u8]) -> Option<Self> {
        let answer: Value = serde_json::from_slice(body).ok()?;
        let mut options = Self::default();
        for option in answer.get("options")?.as_array()? {
            let (Some(name), Some(value)) = (
                option.get("name").and_then(Value::as_str),
                option.get("currentValue").and_then(Value::as_str),
            ) else {
                continue;
            };
            options.take(name, value);
        }
        Some(options)
    }

    /// What a conversation update (`body`) tells of them: its `meetingDetails.meetingCapability`'s
    /// `autoAdmittedUsers`, `presenterOption` and `attendeeRestrictions`, each where it has it; `None` without any.
    #[must_use]
    pub fn of_capability(body: &Value) -> Option<Self> {
        let capability = body.pointer("/meetingDetails/meetingCapability")?.as_object()?;
        let mut options = Self::default();
        for (field, name) in [
            ("autoAdmittedUsers", "AutoAdmittedUsers"),
            ("presenterOption", "PresenterOption"),
            ("attendeeRestrictions", "AttendeeRestrictions"),
        ] {
            if let Some(value) = capability.get(field).and_then(Value::as_str) {
                options.take(name, value);
            }
        }
        (options != Self::default()).then_some(options)
    }

    /// These options with each that `newer` tells over them.
    #[must_use]
    pub fn updated(self, newer: Self) -> Self {
        Self {
            lobby: newer.lobby.or(self.lobby),
            present: newer.present.or(self.present),
            chat: newer.chat.or(self.chat),
            unmute: newer.unmute.or(self.unmute),
            camera: newer.camera.or(self.camera),
        }
    }

    /// Takes the option `name`'s value `value`, when it is one Controls shows in a form it knows.
    fn take(&mut self, name: &str, value: &str) {
        match name {
            "AutoAdmittedUsers" => self.lobby = Some(value != "Everyone"),
            "PresenterOption" => self.present = Some(matches!(value, "Everyone" | "EveryoneInCompany")),
            "AllowMeetingChat" => match value {
                "Enabled" | "InMeeting" => self.chat = Some(true),
                "Disabled" => self.chat = Some(false),
                _ => {}
            },
            "AttendeeRestrictions" => {
                let flags: Vec<&str> = value.split(',').map(str::trim).collect();
                let known = flags
                    .iter()
                    .all(|flag| matches!(*flag, "UnRestricted" | "DisableAudio" | "DisableVideo" | "DisableAv"));
                if known {
                    let has = |flag: &str| flags.contains(&flag) || flags.contains(&"DisableAv");
                    self.unmute = Some(!has("DisableAudio"));
                    self.camera = Some(!has("DisableVideo"));
                }
            }
            _ => {}
        }
    }
}

/// One change Controls makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeetingOption {
    /// Enable lobby on or off.
    Lobby(bool),
    /// Allow all participants to present, or only the organizer.
    Present(bool),
    /// Allow all participants to chat, or not.
    Chat(bool),
    /// What attendees may do with their microphone (`unmute`) and their camera (`camera`): both are one option, so
    /// each change names both.
    Attendees { unmute: bool, camera: bool },
}

impl MeetingOption {
    /// The option's name, type and the value it sets, as Teams' Controls send them.
    fn wire(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::Lobby(on) => (
                "AutoAdmittedUsers",
                "List",
                if on { "EveryoneInCompany" } else { "Everyone" },
            ),
            Self::Present(on) => ("PresenterOption", "List", if on { "Everyone" } else { "OrganizerOnly" }),
            Self::Chat(on) => ("AllowMeetingChat", "List", if on { "Enabled" } else { "Disabled" }),
            Self::Attendees { unmute, camera } => (
                "AttendeeRestrictions",
                "BitMask",
                match (unmute, camera) {
                    (true, true) => "UnRestricted",
                    (false, true) => "DisableAudio",
                    (true, false) => "DisableVideo",
                    (false, false) => "DisableAudio,DisableVideo",
                },
            ),
        }
    }
}

/// `GET beta/meetings/v1/options/<tenant>/<organizer>/<chat>/0/0?monarch=true` on the middle tier: the call's meeting
/// options, as Controls read them when it opens. Its answer is read with [`MeetingOptions::read_answer`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeetingOptionsRead(pub OptionsOf);

impl Read for MeetingOptionsRead {
    fn service(&self) -> Service {
        Service::MiddleTier
    }

    fn endpoint(&self) -> Endpoint {
        self.0.endpoint()
    }
}

/// `POST` of one option to the same resource ([`MeetingOptionsRead`]): Controls' change, answered `200` with the option
/// as it now is ([`MeetingOptions::read_answer`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetMeetingOption {
    of: OptionsOf,
    option: MeetingOption,
}

impl SetMeetingOption {
    #[must_use]
    pub fn new(of: OptionsOf, option: MeetingOption) -> Self {
        Self { of, option }
    }

    /// The change it makes.
    #[must_use]
    pub fn option(&self) -> MeetingOption {
        self.option
    }
}

impl Prepared for SetMeetingOption {
    fn wire(&self) -> Option<Wire> {
        let (name, kind, value) = self.option.wire();
        Some(Wire {
            raw: None,
            service: Service::MiddleTier,
            method: Method::Post,
            endpoint: self.of.endpoint(),
            headers: Vec::new(),
            body: Some(json!({"options": [{"name": name, "type": kind, "currentValue": value}]})),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Request;

    const TENANT: &str = "00000000-0000-4000-8000-0000000000aa";
    const ORGANIZER: &str = "00000000-0000-4000-8000-00000000000a";
    const CHAT: &str = "19:00000000000000000000000000010001@thread.v2";

    fn of() -> OptionsOf {
        OptionsOf::new(TENANT, ORGANIZER, CHAT).unwrap()
    }

    #[test]
    fn the_options_are_the_tenant_the_organizer_and_the_chat_with_monarch() {
        let read = MeetingOptionsRead(of());
        assert_eq!(read.service(), Service::MiddleTier);
        let endpoint = read.endpoint();
        assert_eq!(
            endpoint.segments(),
            ["beta", "meetings", "v1", "options", TENANT, ORGANIZER, CHAT, "0", "0"]
        );
        assert_eq!(endpoint.query(), [("monarch".to_owned(), "true".to_owned())]);
        assert!(OptionsOf::new("tenant", ORGANIZER, CHAT).is_none());
        assert!(OptionsOf::new(TENANT, "organizer", CHAT).is_none());
        assert!(OptionsOf::new(TENANT, ORGANIZER, "48:notes").is_none());
        assert!(OptionsOf::new(TENANT, ORGANIZER, "19:meeting_abc@thread.v2").is_some());
    }

    #[test]
    fn each_change_posts_its_option_as_controls_send_it() {
        let cases = [
            (MeetingOption::Lobby(false), "AutoAdmittedUsers", "List", "Everyone"),
            (
                MeetingOption::Lobby(true),
                "AutoAdmittedUsers",
                "List",
                "EveryoneInCompany",
            ),
            (
                MeetingOption::Present(false),
                "PresenterOption",
                "List",
                "OrganizerOnly",
            ),
            (MeetingOption::Present(true), "PresenterOption", "List", "Everyone"),
            (MeetingOption::Chat(false), "AllowMeetingChat", "List", "Disabled"),
            (MeetingOption::Chat(true), "AllowMeetingChat", "List", "Enabled"),
            (
                MeetingOption::Attendees {
                    unmute: true,
                    camera: true,
                },
                "AttendeeRestrictions",
                "BitMask",
                "UnRestricted",
            ),
            (
                MeetingOption::Attendees {
                    unmute: false,
                    camera: true,
                },
                "AttendeeRestrictions",
                "BitMask",
                "DisableAudio",
            ),
            (
                MeetingOption::Attendees {
                    unmute: true,
                    camera: false,
                },
                "AttendeeRestrictions",
                "BitMask",
                "DisableVideo",
            ),
            (
                MeetingOption::Attendees {
                    unmute: false,
                    camera: false,
                },
                "AttendeeRestrictions",
                "BitMask",
                "DisableAudio,DisableVideo",
            ),
        ];
        for (option, name, kind, value) in cases {
            let write = SetMeetingOption::new(of(), option);
            assert_eq!(write.option(), option);
            assert_eq!(write.service(), Service::MiddleTier);
            assert_eq!(write.method(), Method::Post);
            assert!(write.is_write());
            assert_eq!(write.endpoint(), MeetingOptionsRead(of()).endpoint());
            assert_eq!(
                write.body(),
                Some(json!({"options": [{"name": name, "type": kind, "currentValue": value}]}))
            );
        }
    }

    #[test]
    fn a_read_gives_each_option_controls_show() {
        let body = json!({"options": [
            {"name": "AutoAdmittedUsers", "type": "List", "currentValue": "EveryoneInCompany"},
            {"name": "AllowPstnUsersToBypassLobby", "type": "Boolean", "currentValue": false},
            {"name": "PresenterOption", "type": "List", "currentValue": "Everyone"},
            {"name": "AttendeeRestrictions", "type": "BitMask", "currentValue": "UnRestricted"},
            {"name": "AllowMeetingChat", "type": "List", "currentValue": "Enabled"},
            {"name": "CoOrganizers", "type": "PeopleSelection"}
        ]});
        let options = MeetingOptions::read_answer(body.to_string().as_bytes()).unwrap();
        assert_eq!(
            options,
            MeetingOptions {
                lobby: Some(true),
                present: Some(true),
                chat: Some(true),
                unmute: Some(true),
                camera: Some(true),
            }
        );
        let changed = json!({"success": true, "options": [
            {"name": "AttendeeRestrictions", "currentValue": "DisableAudio,DisableVideo"},
            {"name": "AllowMeetingChat", "currentValue": "Disabled"},
            {"name": "PresenterOption", "currentValue": "OrganizerOnly"},
            {"name": "AutoAdmittedUsers", "currentValue": "Everyone"}
        ]});
        let changed = MeetingOptions::read_answer(changed.to_string().as_bytes()).unwrap();
        assert_eq!(
            changed,
            MeetingOptions {
                lobby: Some(false),
                present: Some(false),
                chat: Some(false),
                unmute: Some(false),
                camera: Some(false),
            }
        );
        assert_eq!(options.updated(MeetingOptions::default()), options);
        assert_eq!(options.updated(changed), changed);
        assert!(MeetingOptions::read_answer(b"{}").is_none());
        assert!(MeetingOptions::read_answer(b"not json").is_none());
        let odd = json!({"options": [
            {"name": "AllowMeetingChat", "currentValue": "Sometimes"},
            {"name": "AttendeeRestrictions", "currentValue": "DisableEverything"}
        ]});
        assert_eq!(
            MeetingOptions::read_answer(odd.to_string().as_bytes()),
            Some(MeetingOptions::default())
        );
    }

    #[test]
    fn a_conversation_update_tells_the_capability() {
        let update = |capability: Value| json!({"meetingDetails": {"meetingCapability": capability}});
        let told = MeetingOptions::of_capability(&update(json!({
            "autoAdmittedUsers": "Everyone",
            "presenterOption": "OrganizerOnly",
            "attendeeRestrictions": "DisableAv",
        })))
        .unwrap();
        assert_eq!(
            told,
            MeetingOptions {
                lobby: Some(false),
                present: Some(false),
                chat: None,
                unmute: Some(false),
                camera: Some(false),
            }
        );
        let video = MeetingOptions::of_capability(&update(json!({"attendeeRestrictions": "DisableVideo"}))).unwrap();
        assert_eq!((video.unmute, video.camera), (Some(true), Some(false)));
        assert!(MeetingOptions::of_capability(&update(json!({"lockMeeting": true}))).is_none());
        assert!(MeetingOptions::of_capability(&json!({"sequenceNumber": 3})).is_none());
    }
}
