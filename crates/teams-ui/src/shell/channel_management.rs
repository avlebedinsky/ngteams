//! The channel menu's choices, shared by channels in the team tree and in account-owned sections.
use super::*;
use crate::channel_menu::ChannelChoice;
use teams_proto::{ChannelSectionAction, OwnTeamView};
use teams_store::{Team, TeamChannel};

impl Shell {
    pub fn channel_chosen(
        &mut self,
        team: Team,
        channel: TeamChannel,
        choice: ChannelChoice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self
            .teams
            .read(cx)
            .teams()
            .and_then(|teams| teams.iter().find(|known| known.id == team.id))
            .and_then(|team| {
                team.channels
                    .iter()
                    .find(|known| known.id == channel.id)
                    .map(|channel| (team.clone(), channel.clone()))
            });
        let Some((team, channel)) = current else {
            return;
        };
        // The visible role is checked again here; the engine reads the authoritative context before writing.
        let allowed = crate::channel_menu::sections(&team, &channel, self.core.read_only())
            .into_iter()
            .flatten()
            .any(|offered| offered.key() == choice.key());
        if !allowed && !(matches!(choice, ChannelChoice::NewSection) && !self.core.read_only()) {
            return;
        }
        if let Some(mode) = crate::channel_management::mode(&choice) {
            let core = Arc::clone(&self.core);
            let dialog =
                cx.new(|cx| crate::channel_management::ChannelManagement::new(core, team, channel, mode, window, cx));
            self.show_team_dialog(&dialog, window, cx);
            return;
        }
        match choice {
            ChannelChoice::CopyLink => {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(crate::channel_menu::channel_link(
                    &team, &channel,
                )));
                self.show_toast("Link copied", cx);
            }
            ChannelChoice::Hide | ChannelChoice::Show => {
                let shown = choice == ChannelChoice::Show;
                let reply = self.core.own_team_view(
                    vec![
                        OwnTeamView::ChannelInterest {
                            channel: channel.id.clone(),
                            own: self.core.me().to_owned(),
                            shown,
                        },
                        OwnTeamView::ChannelFavorite {
                            channel: channel.id.clone(),
                            general: channel.general,
                            shown,
                        },
                    ],
                    Some(teams_core::TeamChange::ChannelShown {
                        team: team.id,
                        channel: channel.id,
                        shown,
                    }),
                );
                Self::toast_after(
                    reply,
                    "Couldn't change channel visibility. Refresh the team and try again.",
                    None,
                    cx,
                );
            }
            ChannelChoice::Move(section) => {
                let reply = self.core.channel_section(ChannelSectionAction::Move {
                    channel: channel.id,
                    section,
                });
                cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                    let result = reply.await;
                    let _ = this.update(cx, |this, cx| {
                        if result.is_ok() {
                            this.teams.update(cx, TeamsList::reload);
                            this.show_toast("Channel moved", cx);
                        } else {
                            this.show_toast("Couldn't move the channel. Refresh your sections and try again.", cx);
                        }
                    });
                })
                .detach();
            }
            ChannelChoice::Workflows => {
                let core = Arc::clone(&self.core);
                let dialog = cx.new(|cx| crate::workflow_dialog::WorkflowDialog::new(core, team, channel, window, cx));
                self.show_team_dialog(&dialog, window, cx);
            }
            _ => {}
        }
    }
}
