//! The channel ellipsis menu, including the account's Move to section submenu.
use crate::icons::Glyph;
use crate::theme::Palette;
use gpui::{App, Context, InteractiveElement as _, ParentElement as _, SharedString, Styled as _, Window, div, px};
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use teams_proto::ChannelSection;
use teams_store::{ChannelKind, Team, TeamChannel};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChannelChoice {
    Notifications,
    Move(String),
    NewSection,
    Hide,
    Show,
    Edit,
    Manage,
    CopyLink,
    Email,
    Workflows,
    Archive,
    Restore,
    Delete,
}

impl ChannelChoice {
    #[must_use]
    pub fn key(&self) -> &'static str {
        match self {
            Self::Notifications => "notifications",
            Self::Move(_) => "move",
            Self::NewSection => "new-section",
            Self::Hide => "hide",
            Self::Show => "show",
            Self::Edit => "edit",
            Self::Manage => "manage",
            Self::CopyLink => "copy-link",
            Self::Email => "email",
            Self::Workflows => "workflows",
            Self::Archive => "archive",
            Self::Restore => "restore",
            Self::Delete => "delete",
        }
    }
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Notifications => "Channel notifications",
            Self::Move(_) => "Move to section",
            Self::NewSection => "New section",
            Self::Hide => "Hide",
            Self::Show => "Show",
            Self::Edit => "Edit channel",
            Self::Manage => "Manage channel",
            Self::CopyLink => "Copy link",
            Self::Email => "Get email address",
            Self::Workflows => "Workflows",
            Self::Archive => "Archive channel",
            Self::Restore => "Restore channel",
            Self::Delete => "Delete channel",
        }
    }
    fn glyph(&self) -> Glyph {
        match self {
            Self::Notifications => Glyph::BELL,
            Self::Move(_) | Self::NewSection => Glyph::STAR,
            Self::Hide => Glyph::EYE_OFF,
            Self::Show => Glyph::EYE,
            Self::Edit => Glyph::PENCIL,
            Self::Manage => Glyph::SETTINGS,
            Self::CopyLink => Glyph::LINK,
            Self::Email => Glyph::MAIL,
            Self::Workflows => Glyph::WORKFLOW,
            Self::Archive => Glyph::ARCHIVE,
            Self::Restore => Glyph::ARCHIVE_RESTORE,
            Self::Delete => Glyph::TRASH,
        }
    }
}

#[must_use]
pub fn can_manage(team: &Team, channel: &TeamChannel) -> bool {
    match channel.kind {
        ChannelKind::Standard => team.can_administer(),
        ChannelKind::Private | ChannelKind::Shared => channel.about.owner,
    }
}

#[must_use]
pub fn can_edit(team: &Team, channel: &TeamChannel) -> bool {
    !team.archived && !channel.about.archived && (can_manage(team, channel) || channel.about.can_edit)
}

/// Teams omits actions a role or channel type cannot use. General cannot be archived or deleted.
#[must_use]
pub fn sections(team: &Team, channel: &TeamChannel, read_only: bool) -> Vec<Vec<ChannelChoice>> {
    let mut view = Vec::new();
    if !read_only {
        view.extend([
            ChannelChoice::Notifications,
            ChannelChoice::Move(String::new()),
            if channel.shown {
                ChannelChoice::Hide
            } else {
                ChannelChoice::Show
            },
        ]);
    }
    let mut manage = Vec::new();
    if !read_only && can_edit(team, channel) {
        manage.push(ChannelChoice::Edit);
    }
    manage.extend([ChannelChoice::Manage, ChannelChoice::CopyLink]);
    if !read_only
        && channel.kind == ChannelKind::Standard
        && !team.about.guest
        && !team.archived
        && !channel.about.archived
    {
        manage.push(ChannelChoice::Email);
    }
    if !team.archived && !channel.about.archived {
        manage.push(ChannelChoice::Workflows);
    }
    let mut destructive = Vec::new();
    if !read_only && !channel.general && can_manage(team, channel) && !team.archived {
        destructive.push(if channel.about.archived {
            ChannelChoice::Restore
        } else {
            ChannelChoice::Archive
        });
    }
    if !read_only
        && !team.archived
        && !channel.general
        && (can_manage(team, channel) || channel.about.can_delete && channel.kind == ChannelKind::Standard)
    {
        destructive.push(ChannelChoice::Delete);
    }
    [view, manage, destructive]
        .into_iter()
        .filter(|section| !section.is_empty())
        .collect()
}

/// A scrollable menu; the kit owns arrow navigation, Escape, focus return, and the nested submenu.
#[derive(Clone, Copy)]
pub struct MenuContext<'a> {
    pub team: &'a Team,
    pub channel: &'a TeamChannel,
    pub read_only: bool,
    pub folders: &'a [ChannelSection],
    pub index: usize,
    pub palette: Palette,
}

pub fn build(
    mut menu: PopupMenu,
    context: MenuContext<'_>,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
    choose: impl Fn(ChannelChoice, &mut Window, &mut App) + Clone + 'static,
) -> PopupMenu {
    let MenuContext {
        team,
        channel,
        read_only,
        folders,
        index,
        palette,
    } = context;
    for (at, section) in sections(team, channel, read_only).into_iter().enumerate() {
        if at > 0 {
            menu = menu.separator();
        }
        for choice in section {
            if matches!(choice, ChannelChoice::Move(_)) {
                let folders = folders.to_vec();
                let chosen = choose.clone();
                let id = channel.id.clone();
                menu = menu.submenu("Move to section", window, cx, move |submenu, window, _| {
                    move_menu(submenu, &folders, &id, (index, palette), window, chosen.clone())
                });
            } else {
                let chosen = choose.clone();
                let label = choice.label();
                let glyph = choice.glyph();
                menu = menu.item(
                    item(format!("channel-{}-{index}", choice.key()), label, glyph, palette)
                        .on_click(move |_, window, cx| chosen(choice.clone(), window, cx)),
                );
            }
        }
    }
    menu.scrollable(true).max_h(window.viewport_size().height - px(16.0))
}

fn move_menu(
    mut submenu: PopupMenu,
    folders: &[ChannelSection],
    id: &str,
    appearance: (usize, Palette),
    window: &mut Window,
    chosen: impl Fn(ChannelChoice, &mut Window, &mut App) + Clone + 'static,
) -> PopupMenu {
    let (index, palette) = appearance;
    for (at, folder) in folders.iter().enumerate() {
        let chosen = chosen.clone();
        let section = folder.id.clone();
        let current = folder.items.iter().any(|item| item == id);
        submenu = submenu.item(
            item(
                format!("channel-section-{index}-{at}"),
                &folder.name,
                Glyph::STAR,
                palette,
            )
            .checked(folder.items.iter().any(|item| item == id))
            .on_click(move |_, window, cx| {
                if !current {
                    chosen(ChannelChoice::Move(section.clone()), window, cx);
                }
            }),
        );
    }
    let chosen = chosen.clone();
    submenu
        .separator()
        .item(
            item(
                format!("channel-new-section-{index}"),
                "New section",
                Glyph::PLUS,
                palette,
            )
            .on_click(move |_, window, cx| chosen(ChannelChoice::NewSection, window, cx)),
        )
        .scrollable(true)
        .max_h(window.viewport_size().height - px(16.0))
}

fn item(id: String, label: &str, glyph: Glyph, palette: Palette) -> PopupMenuItem {
    let label = SharedString::from(label.to_owned());
    PopupMenuItem::element(move |_, _| {
        div()
            .debug_selector({
                let id = id.clone();
                move || id.clone()
            })
            .w_full()
            .flex()
            .items_center()
            .gap_3()
            .px_2()
            .py_1p5()
            .text_color(gpui::rgb(palette.foreground))
            .child(glyph.icon().size(px(16.0)))
            .child(label.clone())
    })
}

/// Teams' channel deep link, with each value encoded independently.
#[must_use]
pub fn channel_link(team: &Team, channel: &TeamChannel) -> String {
    let encode = |value: &str| {
        use std::fmt::Write as _;
        value.bytes().fold(String::new(), |mut out, byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
                out.push(char::from(byte));
            } else {
                let _ = write!(out, "%{byte:02X}");
            }
            out
        })
    };
    format!(
        "https://teams.cloud.microsoft/l/channel/{}/{}?groupId={}&tenantId={}",
        encode(&channel.id),
        encode(&channel.name),
        encode(team.group_id.as_deref().unwrap_or_default()),
        encode(&team.about.tenant)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn team() -> Team {
        Team {
            owner: true,
            id: "19:synthetic team@thread.tacv2".to_owned(),
            group_id: Some("group&x".to_owned()),
            about: teams_store::TeamAbout {
                tenant: "tenant".to_owned(),
                ..teams_store::TeamAbout::default()
            },
            ..Team::default()
        }
    }
    fn channel() -> TeamChannel {
        TeamChannel {
            id: "19:channel@thread.tacv2".to_owned(),
            name: "Synthetic & channel".to_owned(),
            general: false,
            kind: ChannelKind::Standard,
            shown: true,
            about: teams_store::ChannelAbout::default(),
        }
    }
    #[test]
    fn standard_owner_menu_has_every_reference_action_in_order() {
        let entries = sections(&team(), &channel(), false)
            .into_iter()
            .flatten()
            .map(|choice| choice.key())
            .collect::<Vec<_>>();
        assert_eq!(
            entries,
            [
                "notifications",
                "move",
                "hide",
                "edit",
                "manage",
                "copy-link",
                "email",
                "workflows",
                "archive",
                "delete"
            ]
        );
    }
    #[test]
    fn general_private_guest_and_archived_permissions_are_distinct() {
        let team = team();
        let mut channel = channel();
        channel.general = true;
        let entries = sections(&team, &channel, false)
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        assert!(!entries.contains(&ChannelChoice::Archive));
        assert!(!entries.contains(&ChannelChoice::Delete));
        channel.general = false;
        channel.kind = ChannelKind::Private;
        assert!(
            !can_manage(&team, &channel),
            "a team owner doesn't automatically own a private channel"
        );
        channel.about.owner = true;
        channel.about.archived = true;
        let entries = sections(&team, &channel, false)
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        assert!(entries.contains(&ChannelChoice::Restore));
        assert!(!entries.contains(&ChannelChoice::Email));
        assert!(!entries.contains(&ChannelChoice::Edit));
    }
    #[test]
    fn read_only_offers_reads_and_never_preferences_or_admin_actions() {
        let entries = sections(&team(), &channel(), true)
            .into_iter()
            .flatten()
            .map(|choice| choice.key())
            .collect::<Vec<_>>();
        assert_eq!(entries, ["manage", "copy-link", "workflows"]);
    }
    #[test]
    fn channel_links_encode_path_and_query_values_independently() {
        assert_eq!(
            channel_link(&team(), &channel()),
            "https://teams.cloud.microsoft/l/channel/19%3Achannel%40thread.tacv2/Synthetic%20%26%20channel?groupId=group%26x&tenantId=tenant"
        );
    }
}
