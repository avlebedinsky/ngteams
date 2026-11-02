//! A team's "..." menu, the same in the sidebar and on "Your teams and channels", as Teams has it
//! (`docs/protocol/teams.md`): Hide all channels (Show on a hidden team); Add member, Add channel, Manage team, Manage tags,
//! Copy link, Archive (Restore on an archived team); Leave team, Delete team. What a role can't use isn't offered, as Teams
//! decides it ([`sections`]). The read-only mode offers only what writes nothing. Choosing an entry is the caller's to act
//! on: `Shell::team_chosen`.

use std::fmt::Write as _;

use gpui::{
    AnyElement, App, Deferred, FocusHandle, InteractiveElement as _, IntoElement, KeyDownEvent, MouseDownEvent,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px, rgb,
};
use teams_store::Team;

use crate::icons::Glyph;
use crate::shell::menu_row;
use crate::theme::Palette;

/// An entry of a team's "..." menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TeamChoice {
    HideAll,
    Show,
    AddMember,
    AddChannel,
    Manage,
    ManageTags,
    CopyLink,
    Archive,
    Restore,
    Leave,
    Delete,
}

impl TeamChoice {
    /// Its part of the test selector, `team-<key>-<index>`.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::HideAll => "hide-all",
            Self::Show => "show",
            Self::AddMember => "add-member",
            Self::AddChannel => "add-channel",
            Self::Manage => "manage",
            Self::ManageTags => "manage-tags",
            Self::CopyLink => "copy-link",
            Self::Archive => "archive",
            Self::Restore => "restore",
            Self::Leave => "leave",
            Self::Delete => "delete",
        }
    }

    /// Its words, Teams'.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::HideAll => "Hide all channels",
            Self::Show => "Show",
            Self::AddMember => "Add member",
            Self::AddChannel => "Add channel",
            Self::Manage => "Manage team",
            Self::ManageTags => "Manage tags",
            Self::CopyLink => "Copy link",
            Self::Archive => "Archive",
            Self::Restore => "Restore",
            Self::Leave => "Leave team",
            Self::Delete => "Delete team",
        }
    }

    fn glyph(self) -> Glyph {
        match self {
            Self::HideAll => Glyph::EYE_OFF,
            Self::Show => Glyph::EYE,
            Self::AddMember => Glyph::USER_PLUS,
            Self::AddChannel => Glyph::ADD_CHANNEL,
            Self::Manage => Glyph::SETTINGS,
            Self::ManageTags => Glyph::TAG,
            Self::CopyLink => Glyph::LINK,
            Self::Archive => Glyph::ARCHIVE,
            Self::Restore => Glyph::ARCHIVE_RESTORE,
            Self::Leave => Glyph::LOG_OUT,
            Self::Delete => Glyph::TRASH,
        }
    }
}

/// What the menu offers on `team`, in its sections, as the web client decides for the account's role: everyone manages,
/// tags and links; hiding unless it is hidden; adding people and channels and leaving by what the account may
/// ([`Team::can_add_member`], [`Team::can_add_channel`], [`Team::can_leave`]); archiving and deleting for an owner of the
/// whole team. Whatever writes goes in the read-only mode.
#[must_use]
pub fn sections(team: &Team, read_only: bool) -> Vec<Vec<TeamChoice>> {
    let writes = !read_only;
    let mut view = Vec::new();
    if writes {
        view.push(if team.about.hidden {
            TeamChoice::Show
        } else {
            TeamChoice::HideAll
        });
    }
    let mut manage = Vec::new();
    if writes && team.can_add_member() {
        manage.push(TeamChoice::AddMember);
    }
    if writes && team.can_add_channel {
        manage.push(TeamChoice::AddChannel);
    }
    manage.extend([TeamChoice::Manage, TeamChoice::ManageTags, TeamChoice::CopyLink]);
    if writes && team.can_administer() {
        manage.push(if team.archived {
            TeamChoice::Restore
        } else {
            TeamChoice::Archive
        });
    }
    let mut leave = Vec::new();
    if writes && team.can_leave() {
        leave.push(TeamChoice::Leave);
    }
    if writes && team.can_administer() {
        leave.push(TeamChoice::Delete);
    }
    [view, manage, leave]
        .into_iter()
        .filter(|section| !section.is_empty())
        .collect()
}

/// The menu's entries in order.
#[must_use]
pub fn entries(team: &Team, read_only: bool) -> Vec<TeamChoice> {
    sections(team, read_only).into_iter().flatten().collect()
}

/// What a key does to the menu ([`key`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuKey {
    /// Escape or Tab: the menu is to close.
    Close,
    /// Enter or Space on an entry.
    Choose(TeamChoice),
    /// The highlight moved, or the key does nothing here.
    Stay,
}

/// The menu's keys on `highlight`, the entry the arrows are on.
pub fn key(event: &KeyDownEvent, shown: &[TeamChoice], highlight: &mut Option<usize>) -> MenuKey {
    let last = shown.len().saturating_sub(1);
    match event.keystroke.key.as_str() {
        // Tab leaves a menu as Escape does, as Teams' menus take it
        "escape" | "tab" => return MenuKey::Close,
        "down" => *highlight = Some(highlight.map_or(0, |at| if at >= last { 0 } else { at + 1 })),
        "up" => *highlight = Some(highlight.map_or(last, |at| if at == 0 { last } else { at - 1 })),
        "home" => *highlight = Some(0),
        "end" => *highlight = Some(last),
        "enter" | "space" => {
            return shown
                .get(highlight.unwrap_or(0))
                .copied()
                .map_or(MenuKey::Stay, MenuKey::Choose);
        }
        _ => {}
    }
    MenuKey::Stay
}

/// The menu as drawn over everything, its test selector `team-menu-<index>`, its rows `team-<key>-<index>`.
pub struct Panel<'a> {
    pub index: usize,
    pub team: &'a Team,
    pub read_only: bool,
    pub highlight: Option<usize>,
    pub focus: &'a FocusHandle,
    pub palette: Palette,
    /// The corner of the menu that meets what opened it: the sidebar's opens to the right of its "...", the page's to the
    /// left, as Teams opens them.
    pub anchor: gpui::Anchor,
}

impl Panel<'_> {
    /// Draws it: `choose` is told the entry clicked, `close` a click outside.
    pub fn render(
        self,
        choose: impl Fn(TeamChoice, &mut Window, &mut App) + Clone + 'static,
        close: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
        keys: impl Fn(&KeyDownEvent, &mut Window, &mut App) + 'static,
    ) -> Deferred {
        let Self {
            index,
            team,
            read_only,
            highlight,
            focus,
            palette,
            anchor,
        } = self;
        let mut at = 0;
        let mut rows: Vec<AnyElement> = Vec::new();
        for (section_at, section) in sections(team, read_only).into_iter().enumerate() {
            if section_at > 0 {
                rows.push(div().h(px(1.0)).my_1().bg(rgb(palette.border)).into_any_element());
            }
            for choice in section {
                let choose = choose.clone();
                let lit = highlight == Some(at);
                rows.push(
                    menu_row(SharedString::from(format!("team-{}-{index}", choice.key())), palette)
                        .when_lit(lit, palette)
                        .on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            choose(choice, window, cx);
                        })
                        .child(choice.glyph().icon().size(px(16.0)))
                        .child(choice.label())
                        .into_any_element(),
                );
                at += 1;
            }
        }
        gpui::deferred(
            gpui::anchored()
                .anchor(anchor)
                .snap_to_window_with_margin(px(8.0))
                .child(
                    div()
                        .debug_selector(move || format!("team-menu-{index}"))
                        .track_focus(focus)
                        .occlude()
                        .w(px(220.0))
                        .mt_1()
                        .p_1()
                        .flex()
                        .flex_col()
                        .rounded_lg()
                        .border_1()
                        .border_color(rgb(palette.border))
                        .bg(rgb(palette.surface))
                        .shadow_lg()
                        .text_color(rgb(palette.foreground))
                        .on_mouse_down_out(close)
                        .on_key_down(keys)
                        .children(rows),
                ),
        )
        .with_priority(1)
    }
}

/// A row lit by the arrow keys.
trait WhenLit {
    fn when_lit(self, lit: bool, palette: Palette) -> Self;
}

impl WhenLit for gpui::Stateful<gpui::Div> {
    fn when_lit(self, lit: bool, palette: Palette) -> Self {
        if lit { self.bg(rgb(palette.hover)) } else { self }
    }
}

/// The link Copy link copies: the web client's link to a team.
#[must_use]
pub fn team_link(team: &Team) -> String {
    let thread = encoded(&team.id);
    let name = encoded(&team.name);
    let group = team.group_id.as_deref().unwrap_or_default();
    format!(
        "https://teams.cloud.microsoft/l/team/{thread}/{name}?groupId={group}&tenantId={}",
        team.about.tenant
    )
}

/// `text` percent-encoded for a path segment, in lower case as the web client's links have it.
fn encoded(text: &str) -> String {
    text.bytes().fold(String::new(), |mut out, byte| {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02x}");
        }
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use teams_store::TeamAbout;

    fn team(owner: bool, about: TeamAbout, archived: bool) -> Team {
        Team {
            id: "19:team@thread.tacv2".to_owned(),
            name: "A team".to_owned(),
            owner,
            archived,
            can_add_channel: !archived,
            group_id: Some("group".to_owned()),
            about,
            ..Team::default()
        }
    }

    #[test]
    fn an_owner_gets_every_entry_in_teams_sections() {
        assert_eq!(
            sections(&team(true, TeamAbout::default(), false), false),
            vec![
                vec![TeamChoice::HideAll],
                vec![
                    TeamChoice::AddMember,
                    TeamChoice::AddChannel,
                    TeamChoice::Manage,
                    TeamChoice::ManageTags,
                    TeamChoice::CopyLink,
                    TeamChoice::Archive,
                ],
                vec![TeamChoice::Leave, TeamChoice::Delete],
            ]
        );
    }

    #[test]
    fn a_member_of_a_team_of_a_rules_people_can_neither_add_nor_leave_as_on_the_web_client() {
        let about = TeamAbout {
            dynamic_membership: true,
            public: true,
            ..TeamAbout::default()
        };
        assert_eq!(
            entries(&team(false, about, false), false),
            vec![
                TeamChoice::HideAll,
                TeamChoice::AddChannel,
                TeamChoice::Manage,
                TeamChoice::ManageTags,
                TeamChoice::CopyLink,
            ]
        );
    }

    #[test]
    fn a_hidden_archived_team_offers_show_and_restore() {
        let about = TeamAbout {
            hidden: true,
            ..TeamAbout::default()
        };
        let shown = entries(&team(true, about, true), false);
        assert!(shown.contains(&TeamChoice::Show));
        assert!(shown.contains(&TeamChoice::Restore));
        assert!(!shown.contains(&TeamChoice::HideAll));
        assert!(!shown.contains(&TeamChoice::AddChannel));
        assert!(!shown.contains(&TeamChoice::AddMember));
    }

    #[test]
    fn the_read_only_mode_offers_nothing_that_writes() {
        assert_eq!(
            entries(&team(true, TeamAbout::default(), false), true),
            vec![TeamChoice::Manage, TeamChoice::ManageTags, TeamChoice::CopyLink]
        );
    }

    #[test]
    fn a_teams_link_is_the_web_clients() {
        let about = TeamAbout {
            tenant: "tenant".to_owned(),
            ..TeamAbout::default()
        };
        assert_eq!(
            team_link(&team(true, about, false)),
            "https://teams.cloud.microsoft/l/team/19%3ateam%40thread.tacv2/A%20team?groupId=group&tenantId=tenant"
        );
    }
}
