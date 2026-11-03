//! Channel dialogs. A form sends only on the user's confirm; failed or uncertain writes stay visible and are never
//! automatically retried. All service operations go through Core, including fresh preference and moderation reads.
mod members;
use crate::bridge::Core;
use crate::channel_menu::{self, ChannelChoice};
use crate::icons::{Glyph, icon_button, selected_by};
use crate::team_dialogs::{self, TeamDialogEvent};
use crate::theme::{self, Palette};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, AsyncApp, Context, Entity, EventEmitter, FocusHandle, InteractiveElement as _, IntoElement,
    KeyDownEvent, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription,
    Task, WeakEntity, Window, div, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_component::switch::Switch;
use gpui_component::{Disableable as _, FocusTrapElement as _, Selectable as _};
use serde_json::{Value, json};
use std::sync::Arc;
use teams_core::{EngineError, TeamChange};
use teams_proto::{
    ChannelChange, ChannelData, ChannelOwnProperty, ChannelRead, ChannelSectionAction, ChannelTarget, ChannelWrite,
    OwnTeamView, TeamWrite,
};
use teams_store::{ChannelKind, Team, TeamChannel};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Edit,
    Manage,
    Notifications,
    Email,
    Archive,
    Restore,
    Delete,
    NewSection,
}
impl Mode {
    fn title(self) -> &'static str {
        match self {
            Self::Edit => "Edit channel",
            Self::Manage => "Manage channel",
            Self::Notifications => "Channel notifications",
            Self::Email => "Get email address",
            Self::Archive => "Archive channel",
            Self::Restore => "Restore channel",
            Self::Delete => "Delete channel",
            Self::NewSection => "New section",
        }
    }
    fn confirm(self) -> &'static str {
        match self {
            Self::Archive => "Archive",
            Self::Restore => "Restore",
            Self::Delete => "Delete",
            Self::NewSection => "Create",
            _ => "Save",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NotificationPreset {
    All,
    #[default]
    MentionsAndReplies,
    Muted,
}

/// Preferences understood by both the dialog and the desktop notifier. Unknown JSON keys are kept on save.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelNotifications {
    pub preset: NotificationPreset,
    pub tag_mentions: bool,
    pub channel_mentions: bool,
    pub desktop: Option<bool>,
    pub follow_threads: bool,
    pub raw: Value,
}
impl Default for ChannelNotifications {
    fn default() -> Self {
        Self {
            preset: NotificationPreset::MentionsAndReplies,
            tag_mentions: true,
            channel_mentions: true,
            desktop: None,
            follow_threads: false,
            raw: json!({}),
        }
    }
}
impl ChannelNotifications {
    #[must_use]
    pub fn read(data: &Value) -> Self {
        let properties = data.get("properties").unwrap_or(data);
        let raw = properties
            .get("channelNotificationSettings")
            .and_then(Value::as_str)
            .and_then(|text| serde_json::from_str::<Value>(text).ok())
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({}));
        Self {
            preset: if properties.get("alerts").and_then(Value::as_str) == Some("false") {
                NotificationPreset::Muted
            } else if raw.get("allNewPosts").and_then(Value::as_str) == Some("On") {
                NotificationPreset::All
            } else {
                NotificationPreset::MentionsAndReplies
            },
            tag_mentions: raw.get("tag").and_then(Value::as_str) != Some("Off"),
            channel_mentions: raw.get("channelTeam").and_then(Value::as_str) != Some("Off"),
            desktop: match raw.get("dskNotif").and_then(Value::as_str) {
                Some("On") => Some(true),
                Some("Off") => Some(false),
                _ => None,
            },
            follow_threads: properties.get("followAllRc").and_then(Value::as_str) == Some("true"),
            raw,
        }
    }
    #[must_use]
    pub fn requests(&self, channel: &str) -> Vec<OwnTeamView> {
        let mut raw = self.raw.clone();
        let on = |enabled| if enabled { "On" } else { "Off" };
        let muted = self.preset == NotificationPreset::Muted;
        raw["allNewPosts"] = json!(on(self.preset == NotificationPreset::All));
        raw["tag"] = json!(on(self.tag_mentions && !muted));
        raw["channelTeam"] = json!(on(self.channel_mentions && !muted));
        raw["dskNotif"] = self.desktop.map_or(Value::Null, |enabled| json!(on(enabled)));
        [
            (ChannelOwnProperty::Notifications, raw.to_string()),
            (ChannelOwnProperty::Alerts, (!muted).to_string()),
            (ChannelOwnProperty::FollowAll, self.follow_threads.to_string()),
        ]
        .into_iter()
        .map(|(property, value)| OwnTeamView::ChannelProperty {
            channel: channel.to_owned(),
            property,
            value,
        })
        .collect()
    }
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "independent form values across channel dialog modes"
)]
pub struct ChannelManagement {
    core: Arc<dyn Core>,
    team: Team,
    channel: TeamChannel,
    mode: Mode,
    name: Entity<InputState>,
    description: Entity<TextareaState>,
    domains: Entity<InputState>,
    focus: FocusHandle,
    loading: bool,
    working: Option<Task<()>>,
    read: Option<Task<()>>,
    error: Option<SharedString>,
    notices: ChannelNotifications,
    moderation: Value,
    team_settings: Value,
    email_domains: Vec<String>,
    email: Option<String>,
    email_allowed: String,
    email_settings: bool,
    site_read_only: bool,
    confirmed: bool,
    members: members::Members,
    manage_members: bool,
    _inputs: [Subscription; 4],
}
impl EventEmitter<TeamDialogEvent> for ChannelManagement {}
impl ChannelManagement {
    pub fn new(
        core: Arc<dyn Core>,
        team: Team,
        channel: TeamChannel,
        mode: Mode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(if mode == Mode::NewSection {
                    "".to_owned()
                } else {
                    channel.name.clone()
                })
                .placeholder("Name")
        });
        let description = cx.new(|cx| {
            TextareaState::new(window, cx)
                .default_value(channel.about.description.clone())
                .auto_grow(3, 5)
                .soft_wrap(true)
        });
        let domains = cx.new(|cx| InputState::new(window, cx).placeholder("example.com, another.example"));
        let member_search = cx.new(|cx| InputState::new(window, cx).placeholder("Type a name or email"));
        let inputs = [
            cx.subscribe(&member_search, |_, _, _: &InputEvent, cx| cx.notify()),
            cx.subscribe(&name, |_, _, _: &InputEvent, cx| cx.notify()),
            cx.subscribe(&description, |_, _, _: &InputEvent, cx| cx.notify()),
            cx.subscribe(&domains, |_, _, _: &InputEvent, cx| cx.notify()),
        ];
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        if matches!(mode, Mode::Edit | Mode::NewSection) {
            name.update(cx, |state, cx| state.focus(window, cx));
        }
        let manage_members = channel.kind != ChannelKind::Standard;
        let mut this = Self {
            core,
            team,
            channel,
            mode,
            name,
            description,
            domains,
            focus,
            loading: false,
            working: None,
            read: None,
            error: None,
            notices: ChannelNotifications::default(),
            moderation: json!({}),
            team_settings: json!({}),
            email_domains: Vec::new(),
            email: None,
            email_allowed: "members".to_owned(),
            email_settings: false,
            site_read_only: false,
            confirmed: false,
            members: members::Members::new(member_search),
            manage_members,
            _inputs: inputs,
        };
        this.reload(cx);
        this
    }
    fn target(&self) -> ChannelTarget {
        ChannelTarget {
            team: self.team.id.clone(),
            team_group: self.team.group_id.clone().unwrap_or_default(),
            channel: self.channel.id.clone(),
            channel_group: if self.channel.kind == ChannelKind::Standard {
                None
            } else {
                self.channel.about.group.as_ref().map(|group| {
                    if group.starts_with("OID:") {
                        group.clone()
                    } else {
                        format!("OID:{group}@{}", self.team.about.tenant)
                    }
                })
            },
            mailbox: self.team.mailbox.clone(),
        }
    }
    fn reload(&mut self, cx: &mut Context<Self>) {
        if self.mode == Mode::Manage {
            self.reload_members(cx);
        }
        let query = match self.mode {
            Mode::Notifications => ChannelRead::Preferences {
                channel: self.channel.id.clone(),
            },
            Mode::Manage => ChannelRead::Thread {
                channel: self.channel.id.clone(),
            },
            Mode::Email => ChannelRead::Email {
                channel: self.channel.id.clone(),
            },
            _ => return,
        };
        self.loading = true;
        self.error = None;
        let reply = self.core.channel_data(query);
        self.read = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let result = reply.await;
            let _ = this.update(cx, |this, cx| this.loaded(result, cx));
        }));
        cx.notify();
    }
    fn loaded(&mut self, result: Result<ChannelData, EngineError>, cx: &mut Context<Self>) {
        self.loading = false;
        match result {
            Ok(data) => {
                match self.mode {
                    Mode::Notifications => self.notices = ChannelNotifications::read(&data.0),
                    Mode::Manage => {
                        let raw = data.0.pointer("/properties/channelSettings");
                        self.team_settings = data
                            .0
                            .pointer("/properties/spaceAdminSettings")
                            .and_then(Value::as_str)
                            .and_then(|text| serde_json::from_str(text).ok())
                            .filter(Value::is_object)
                            .unwrap_or_else(|| json!({}));
                        self.moderation = raw
                        .and_then(Value::as_str)
                        .and_then(|text| serde_json::from_str(text).ok())
                        .or_else(|| raw.filter(|value| value.is_object()).cloned())
                        .unwrap_or_else(|| json!({"postPermissions":0,"allowReplies":0,"allowPinPosts":0,"allowBotsPost":1,"allowConnectorsPost":1}));
                    }
                    Mode::Email => {
                        if data.0.get("errorCode").and_then(Value::as_str) == Some("NotFound") {
                            self.create_email(cx);
                            cx.notify();
                            return;
                        }
                        if data.0.get("errorCode").is_some() {
                            self.error=Some("Channel email is unavailable. Ask your Teams administrator to check email integration.".into());
                        }
                        self.email = data
                            .0
                            .pointer("/emailAddressDetails/emailId")
                            .or_else(|| data.0.get("emailAddress"))
                            .or_else(|| data.0.get("email"))
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                            .map(str::to_owned);
                        self.email_domains = data
                            .0
                            .pointer("/allowedSenders/allowedDomains")
                            .or_else(|| data.0.get("allowedDomains"))
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect();
                        if let Some(allowed) = data
                            .0
                            .pointer("/allowedSenders/allowedSenderType")
                            .or_else(|| data.0.get("allowedSenderType"))
                            .and_then(Value::as_str)
                        {
                            self.email_allowed = allowed.to_owned();
                        }
                    }
                    _ => {}
                }
            }
            Err(_) => self.error = Some("Couldn't load the channel settings. Try again.".into()),
        }
        cx.notify();
    }
    fn close(&self, cx: &mut Context<Self>) {
        if self.working.is_none() {
            cx.emit(TeamDialogEvent::Closed);
        }
    }
    fn problem(&self, cx: &App) -> Option<String> {
        match self.mode {
            Mode::Edit => {
                let name = self.name.read(cx).value().trim().to_owned();
                let taken = self
                    .team
                    .channels
                    .iter()
                    .filter(|row| row.id != self.channel.id)
                    .map(|row| row.name.clone())
                    .collect::<Vec<_>>();
                // General can keep its current name; a renamed channel still follows all ordinary name rules.
                if name != self.channel.name
                    && let Some(problem) = crate::channel_dialog::name_problem(&name, &taken)
                {
                    return Some(problem);
                }
                if name.is_empty() {
                    return Some("Enter a channel name".to_owned());
                }
                crate::channel_dialog::description_problem(self.description.read(cx).value().as_str())
                    .map(str::to_owned)
            }
            Mode::NewSection => {
                let name = self.name.read(cx).value();
                (name.trim().is_empty() || name.chars().count() > 80)
                    .then(|| "Enter a section name of up to 80 characters".to_owned())
            }
            Mode::Email if self.email_allowed == "domains" => {
                let domains = self.domain_list(cx);
                (domains.is_empty() || domains.iter().any(|domain| !valid_domain(domain)))
                    .then(|| "Enter valid domain names separated by commas".to_owned())
            }
            _ => None,
        }
    }
    fn domain_list(&self, cx: &App) -> Vec<String> {
        self.domains
            .read(cx)
            .value()
            .split([',', ';', ' '])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_ascii_lowercase)
            .collect()
    }
    fn apply(&mut self, cx: &mut Context<Self>) {
        if self.core.read_only() || self.loading || self.working.is_some() || self.problem(cx).is_some() {
            return;
        }
        if self.mode == Mode::Delete && !self.confirmed {
            return;
        }
        self.error = None;
        if self.mode == Mode::Notifications {
            let reply = self.core.own_team_view(self.notices.requests(&self.channel.id), None);
            self.finish(reply, "Channel notifications saved", cx);
            return;
        }
        if self.mode == Mode::NewSection {
            let reply = self.core.channel_section(ChannelSectionAction::Create {
                channel: self.channel.id.clone(),
                name: self.name.read(cx).value().trim().to_owned(),
            });
            self.finish(reply, "Channel moved to the new section", cx);
            return;
        }
        if self.mode == Mode::Manage && self.channel.general {
            if !self.team.can_administer() || self.team.archived || self.team_settings.get("userSettings").is_none() {
                return;
            }
            let reply = self.core.team_write(
                TeamWrite::Settings {
                    team: self.team.id.clone(),
                    settings: self.team_settings.to_string(),
                },
                None,
            );
            self.working = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let result = reply.await;
                let _ = this.update(cx, |this, cx| {
                    this.written(result.map(|_| ()), "Channel settings saved", cx)
                });
            }));
            cx.notify();
            return;
        }
        let (action, change, words) = match self.mode {
            Mode::Edit => {
                if !channel_menu::can_edit(&self.team, &self.channel) {
                    return;
                }
                let name = self.name.read(cx).value().trim().to_owned();
                let description = self.description.read(cx).value().trim().to_owned();
                (
                    ChannelChange::Edit {
                        name: name.clone(),
                        description: description.clone(),
                    },
                    Some(TeamChange::ChannelEdited {
                        team: self.team.id.clone(),
                        channel: self.channel.id.clone(),
                        name,
                        description,
                    }),
                    "Channel updated",
                )
            }
            Mode::Manage => {
                if !channel_menu::can_manage(&self.team, &self.channel)
                    || self.team.archived
                    || self.channel.about.archived
                {
                    return;
                }
                (
                    ChannelChange::Settings(self.moderation.clone()),
                    None,
                    "Channel settings saved",
                )
            }
            Mode::Archive | Mode::Restore => {
                if self.channel.general || !channel_menu::can_manage(&self.team, &self.channel) || self.team.archived {
                    return;
                }
                let archived = self.mode == Mode::Archive;
                (
                    ChannelChange::Archive {
                        archived,
                        site_read_only: self.site_read_only,
                    },
                    Some(TeamChange::ChannelArchived {
                        team: self.team.id.clone(),
                        channel: self.channel.id.clone(),
                        archived,
                    }),
                    if archived {
                        "Channel archived"
                    } else {
                        "Channel restored"
                    },
                )
            }
            Mode::Delete => {
                if self.channel.general {
                    return;
                }
                (
                    ChannelChange::Delete,
                    Some(TeamChange::ChannelGone {
                        team: self.team.id.clone(),
                        channel: self.channel.id.clone(),
                    }),
                    "Channel deleted",
                )
            }
            Mode::Email if self.email_settings => (
                ChannelChange::UpdateEmail {
                    allowed_sender_type: self.email_allowed.clone(),
                    allowed_domains: self.domain_list(cx),
                },
                None,
                "Email settings saved",
            ),
            _ => return,
        };
        self.write(action, change, words, cx);
    }
    fn write(
        &mut self,
        action: ChannelChange,
        change: Option<TeamChange>,
        words: &'static str,
        cx: &mut Context<Self>,
    ) {
        if self.channel.kind != ChannelKind::Standard && self.target().channel_group.is_none() {
            self.error =
                Some("Teams hasn't supplied this channel's group yet. Close this dialog and refresh the team.".into());
            cx.notify();
            return;
        }
        let reply = self.core.team_write(
            TeamWrite::Channel(ChannelWrite {
                target: self.target(),
                action,
            }),
            change,
        );
        self.working = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let result = reply.await;
            let _ = this.update(cx, |this, cx| this.written(result.map(|_| ()), words, cx));
        }));
        cx.notify();
    }
    fn finish(&mut self, reply: teams_core::Reply<()>, words: &'static str, cx: &mut Context<Self>) {
        self.working = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let result = reply.await;
            let _ = this.update(cx, |this, cx| this.written(result, words, cx));
        }));
        cx.notify();
    }
    fn written(&mut self, result: Result<(), EngineError>, words: &'static str, cx: &mut Context<Self>) {
        self.working = None;
        match result {
            Ok(()) => cx.emit(TeamDialogEvent::Changed {
                team: self.team.id.clone(),
                words,
            }),
            Err(error) => {
                self.error = Some(
                    match error {
                        EngineError::TeamNotChanged { code: Some(code) }
                            if code.contains("Permission") || code.contains("Forbidden") =>
                        {
                            "You don't have permission to change this channel. Ask a channel owner."
                        }
                        _ => "The channel change wasn't confirmed. Refresh the channel before trying again.",
                    }
                    .into(),
                );
            }
        }
        cx.notify();
    }
    fn form(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        match self.mode {
            Mode::Edit | Mode::NewSection => {
                let form = div().flex().flex_col().gap_2().child("Name").child(selected_by("channel-management-name", Input::new(&self.name)));
                if self.mode == Mode::Edit { form.child("Description").child(selected_by("channel-management-description", Textarea::new(&self.description))) } else { form.child(div().text_sm().text_color(rgb(palette.muted_foreground)).child("Create a section and move this channel into it.")) }
            }
            Mode::Notifications => self.notification_form(palette, cx),
            Mode::Manage => self.manage_form(palette, cx),
            Mode::Email => self.email_form(palette, cx),
            Mode::Archive => div().flex().flex_col().gap_4().child("Archiving freezes channel activity. People can still read and search its conversations and files. You can restore it later.")
                .child(self.switch("channel-archive-site-read-only", "Make the SharePoint site read-only for channel members", self.site_read_only, |this, value| this.site_read_only = value, palette, cx)),
            Mode::Restore => div().child("Restore this channel so members can post and collaborate again?"),
            Mode::Delete => div().flex().flex_col().gap_4().child("This channel's conversations will be deleted. Files remain in SharePoint. Team owners can restore a deleted channel for 30 days.")
                .child(self.switch("channel-delete-confirm", "I understand that this channel will be deleted", self.confirmed, |this, value| this.confirmed = value, palette, cx)),
        }
    }
    fn switch(
        &self,
        id: &'static str,
        label: &'static str,
        checked: bool,
        set: fn(&mut Self, bool),
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let control = Switch::new(id)
            .checked(checked)
            .disabled(self.core.read_only() || self.working.is_some())
            .on_click(cx.listener(move |this, value: &bool, _, cx| {
                set(this, *value);
                cx.notify();
            }));
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_4()
            .py_2()
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(div().flex_1().min_w_0().text_sm().child(label))
            .child(selected_by(id, control))
    }
    fn notification_form(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let presets = [
            ("all", "All new messages", NotificationPreset::All),
            (
                "mentions",
                "Mentions and replies",
                NotificationPreset::MentionsAndReplies,
            ),
            ("muted", "Mute", NotificationPreset::Muted),
        ]
        .into_iter()
        .map(|(key, label, preset)| {
            selected_by(
                format!("channel-notification-{key}"),
                Button::new(SharedString::from(format!("channel-notification-{key}")))
                    .label(label)
                    .selected(self.notices.preset == preset)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.notices.preset = preset;
                        cx.notify();
                    })),
            )
        });
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().text_sm().child("Notify me about"))
            .child(div().flex().flex_wrap().gap_2().children(presets))
            .child(self.switch(
                "channel-notification-follow",
                "Include replies to all threads",
                self.notices.follow_threads,
                |this, v| this.notices.follow_threads = v,
                palette,
                cx,
            ))
            .child(self.switch(
                "channel-notification-mentions",
                "Channel and team mentions",
                self.notices.channel_mentions,
                |this, v| this.notices.channel_mentions = v,
                palette,
                cx,
            ))
            .child(self.switch(
                "channel-notification-tags",
                "Tag mentions",
                self.notices.tag_mentions,
                |this, v| this.notices.tag_mentions = v,
                palette,
                cx,
            ))
            .child(self.switch(
                "channel-notification-desktop",
                "Show desktop notifications",
                self.notices.desktop.unwrap_or(true),
                |this, v| this.notices.desktop = Some(v),
                palette,
                cx,
            ))
    }
    fn manage_form(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let tabs = div()
            .flex()
            .gap_2()
            .mb_2()
            .child(selected_by(
                "channel-manage-settings",
                Button::new("channel-manage-settings")
                    .label("Settings")
                    .selected(!self.manage_members)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.manage_members = false;
                        cx.notify();
                    })),
            ))
            .child(selected_by(
                "channel-manage-members",
                Button::new("channel-manage-members")
                    .label("Members")
                    .selected(self.manage_members)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.manage_members = true;
                        cx.notify();
                    })),
            ));
        let content = if self.manage_members {
            self.member_form(palette, cx)
        } else {
            self.moderation_form(palette, cx)
        };
        div().flex().flex_col().gap_2().child(tabs).child(content)
    }

    fn moderation_form(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let can_manage = channel_menu::can_manage(&self.team, &self.channel)
            && !self.core.read_only()
            && !self.team.archived
            && !self.channel.about.archived;
        let title = div()
            .text_sm()
            .text_color(rgb(palette.muted_foreground))
            .child(match self.channel.kind {
                ChannelKind::Standard => "Standard channel · Everyone on the team has access",
                ChannelKind::Private => "Private channel · Only channel members have access",
                ChannelKind::Shared => "Shared channel · Access is managed by channel owners",
            });
        let form = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(title)
            .child(div().text_lg().child("Channel moderation"));
        if self.channel.general || self.channel.kind != ChannelKind::Standard {
            return form.child(
                div()
                    .text_sm()
                    .child("This channel uses its team's posting permissions."),
            );
        }
        if !can_manage {
            return form.child(
                div()
                    .text_sm()
                    .child("Ask a channel owner to change the moderation settings."),
            );
        }
        form.child(self.switch(
            "channel-moderation-on",
            "Only moderators can start new posts",
            self.moderation.get("postPermissions").and_then(Value::as_u64) == Some(1),
            |this, v| this.moderation["postPermissions"] = json!(u8::from(v)),
            palette,
            cx,
        ))
        .child(self.switch(
            "channel-moderation-replies",
            "Allow members to reply to channel messages",
            self.moderation.get("allowReplies").and_then(Value::as_u64) != Some(1),
            |this, v| this.moderation["allowReplies"] = json!(u8::from(!v)),
            palette,
            cx,
        ))
        .child(self.switch(
            "channel-moderation-pins",
            "Allow members to pin posts",
            self.moderation.get("allowPinPosts").and_then(Value::as_u64) != Some(1),
            |this, v| this.moderation["allowPinPosts"] = json!(u8::from(!v)),
            palette,
            cx,
        ))
        .child(self.switch(
            "channel-moderation-bots",
            "Allow bots to submit channel messages",
            self.moderation.get("allowBotsPost").and_then(Value::as_u64) != Some(0),
            |this, v| this.moderation["allowBotsPost"] = json!(u8::from(v)),
            palette,
            cx,
        ))
        .child(self.switch(
            "channel-moderation-connectors",
            "Allow connectors to submit channel messages",
            self.moderation.get("allowConnectorsPost").and_then(Value::as_u64) != Some(0),
            |this, v| this.moderation["allowConnectorsPost"] = json!(u8::from(v)),
            palette,
            cx,
        ))
    }
    fn email_form(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let mut form = div().flex().flex_col().gap_3();
        if let Some(email) = &self.email {
            let copied = email.clone();
            form = form.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(div().text_sm().child(email.clone()))
                    .child(selected_by(
                        "channel-email-copy",
                        icon_button("channel-email-copy", Glyph::COPY, "Copy email address").on_click(
                            move |_, _, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string(copied.clone())),
                        ),
                    )),
            );
            if channel_menu::can_manage(&self.team, &self.channel) && !self.core.read_only() {
                form = form
                    .child(selected_by(
                        "channel-email-settings",
                        Button::new("channel-email-settings")
                            .label("Advanced settings")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.email_settings = !this.email_settings;
                                cx.notify();
                            })),
                    ))
                    .child(selected_by(
                        "channel-email-remove",
                        Button::new("channel-email-remove")
                            .label("Remove email address")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.write(ChannelChange::RemoveEmail, None, "Email address removed", cx);
                            })),
                    ));
            }
        } else {
            form = form
                .child(div().text_sm().child("This channel doesn't have an email address yet."))
                .child(selected_by(
                    "channel-email-create",
                    Button::new("channel-email-create")
                        .label("Get email address")
                        .primary()
                        .on_click(cx.listener(|this, _, _, cx| this.create_email(cx))),
                ));
        }
        if self.email_settings {
            for (key, label) in [
                ("anyone", "Anyone can send emails to this address"),
                ("members", "Only members of this team"),
                ("domains", "Only email sent from these domains"),
            ] {
                form = form.child(selected_by(
                    format!("channel-email-allowed-{key}"),
                    Button::new(SharedString::from(format!("channel-email-allowed-{key}")))
                        .label(label)
                        .selected(self.email_allowed == key)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.email_allowed = key.to_owned();
                            cx.notify();
                        })),
                ));
            }
            if self.email_allowed == "domains" {
                form = form.child(selected_by("channel-email-domains", Input::new(&self.domains)));
            }
        }
        form.child(
            div()
                .text_sm()
                .text_color(rgb(palette.muted_foreground))
                .child("Email sent to this address appears in the channel."),
        )
    }
    fn create_email(&mut self, cx: &mut Context<Self>) {
        if self.core.read_only() || self.working.is_some() || self.error.is_some() {
            return;
        }
        let reply = self.core.team_write(
            TeamWrite::Channel(ChannelWrite {
                target: self.target(),
                action: ChannelChange::CreateEmail,
            }),
            None,
        );
        self.working = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let result = reply.await;
            let _ = this.update(cx, |this, cx| {
                this.working = None;
                if result.is_ok() {
                    this.reload(cx);
                } else {
                    this.error = Some(
                        "Couldn't create an email address. Your administrator may have disabled channel email.".into(),
                    );
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn footer(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let writes = self.mode != Mode::Email || self.email_settings;
        let manage_write = self.mode != Mode::Manage
            || channel_menu::can_manage(&self.team, &self.channel)
                && self.channel.kind == ChannelKind::Standard
                && !self.manage_members;
        let enabled = self.problem(cx).is_none()
            && self.error.is_none()
            && !self.loading
            && self.working.is_none()
            && (self.mode != Mode::Delete || self.confirmed);
        let footer = div()
            .flex()
            .justify_end()
            .gap_2()
            .pt_3()
            .border_t_1()
            .border_color(rgb(palette.border))
            .child(selected_by(
                "channel-management-cancel",
                Button::new("channel-management-cancel")
                    .label(if writes { "Cancel" } else { "Close" })
                    .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
            ));
        if self.core.read_only() || !writes || !manage_write {
            return footer;
        }
        footer.child(selected_by(
            "channel-management-save",
            Button::new("channel-management-save")
                .label(if self.working.is_some() {
                    "Saving…"
                } else {
                    self.mode.confirm()
                })
                .primary()
                .disabled(!enabled)
                .on_click(cx.listener(|this, _, _, cx| this.apply(cx))),
        ))
    }
}
impl Render for ChannelManagement {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let content = if self.loading {
            team_dialogs::waiting("channel-management-loading", "Loading channel settings…", palette)
        } else {
            self.form(palette, cx)
        };
        team_dialogs::surface("channel-management-dialog", palette, window)
            .track_focus(&self.focus)
            .key_context("TeamDialog")
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    this.close(cx);
                }
            }))
            .child(
                div()
                    .flex()
                    .items_start()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(team_dialogs::title(self.mode.title()))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(palette.muted_foreground))
                                    .child(self.channel.name.clone()),
                            ),
                    )
                    .child(selected_by(
                        "channel-management-close",
                        icon_button("channel-management-close", Glyph::X, "Close")
                            .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                    )),
            )
            .child(
                div()
                    .id("channel-management-scroll")
                    .max_h(px(470.0))
                    .overflow_y_scroll()
                    .child(content),
            )
            .children(
                self.error
                    .as_ref()
                    .map(|words| team_dialogs::problem("channel-management-error", words.clone())),
            )
            .when(
                self.error.is_some()
                    && !self.loading
                    && matches!(self.mode, Mode::Manage | Mode::Notifications | Mode::Email),
                |dialog| {
                    dialog.child(selected_by(
                        "channel-management-reload",
                        Button::new("channel-management-reload")
                            .label("Refresh")
                            .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                    ))
                },
            )
            .children(
                self.problem(cx)
                    .map(|words| team_dialogs::problem("channel-management-validation", words)),
            )
            .child(self.footer(palette, cx))
            .focus_trap("channel-management-trap", &self.focus)
    }
}

fn valid_domain(domain: &str) -> bool {
    domain.len() <= 253
        && domain.contains('.')
        && domain.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

#[must_use]
pub fn mode(choice: &ChannelChoice) -> Option<Mode> {
    Some(match choice {
        ChannelChoice::Notifications => Mode::Notifications,
        ChannelChoice::Edit => Mode::Edit,
        ChannelChoice::Manage => Mode::Manage,
        ChannelChoice::Email => Mode::Email,
        ChannelChoice::Archive => Mode::Archive,
        ChannelChoice::Restore => Mode::Restore,
        ChannelChoice::Delete => Mode::Delete,
        ChannelChoice::NewSection => Mode::NewSection,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use teams_proto::Request;
    #[test]
    fn muted_and_desktop_preferences_are_read_and_unknown_keys_survive_a_save() {
        let properties = json!({"properties":{"alerts":"false","followAllRc":"true","channelNotificationSettings":json!({"allNewPosts":"Off","tag":"On","channelTeam":"Off","dskNotif":"Off","future":{"keep":1}}).to_string()}});
        let mut settings = ChannelNotifications::read(&properties);
        assert_eq!(settings.preset, NotificationPreset::Muted);
        assert_eq!(settings.desktop, Some(false));
        assert!(settings.follow_threads);
        settings.preset = NotificationPreset::All;
        let requests = settings.requests("channel");
        let body = requests[0].body().expect("body");
        let raw: Value =
            serde_json::from_str(body["channelNotificationSettings"].as_str().expect("string")).expect("json");
        assert_eq!(raw["future"], json!({"keep":1}));
        assert_eq!(raw["allNewPosts"], "On");
        assert_eq!(requests[1].body().expect("alerts")["alerts"], "true");
    }
    #[test]
    fn sender_domains_reject_urls_addresses_and_malformed_dns_labels() {
        assert!(valid_domain("example.invalid"));
        assert!(valid_domain("sub-domain.example.invalid"));
        for value in [
            "https://example.invalid",
            "somebody@example.invalid",
            "-bad.example",
            "bad-.example",
            "bad..example",
            "localhost",
        ] {
            assert!(!valid_domain(value), "{value}");
        }
    }
}
