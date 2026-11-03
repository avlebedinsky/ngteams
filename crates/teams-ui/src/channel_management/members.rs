//! Channel members and moderators. Standard channels inherit team membership; private/shared membership uses the
//! channel's own provisioning identity. Failed updates leave the dialog open and refreshes never resend a write.
use super::*;
use gpui::AnyElement;
use teams_core::TeamMember;
use teams_proto::TeamPerson;

pub(super) struct Members {
    pub search: Entity<InputState>,
    pub listed: Vec<TeamMember>,
    pub found: Vec<TeamPerson>,
    pub loading: bool,
    pub searching: bool,
    pub remove: Option<String>,
    pub read: Option<Task<()>>,
    pub search_task: Option<Task<()>>,
}
impl Members {
    pub fn new(search: Entity<InputState>) -> Self {
        Self {
            search,
            listed: Vec::new(),
            found: Vec::new(),
            loading: true,
            searching: false,
            remove: None,
            read: None,
            search_task: None,
        }
    }
}

impl ChannelManagement {
    pub(super) fn reload_members(&mut self, cx: &mut Context<Self>) {
        let thread = if self.channel.kind == ChannelKind::Standard {
            self.team.id.clone()
        } else {
            self.channel.id.clone()
        };
        let reply = self.core.team_roster(thread);
        self.members.loading = true;
        self.members.read = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let result = reply.await;
            let _ = this.update(cx, |this, cx| {
                this.members.loading = false;
                if let Ok(members) = result
                    && !members.is_empty()
                {
                    this.members.listed = members;
                } else {
                    this.error = Some("Couldn't read the channel members. Refresh before changing membership.".into());
                }
                cx.notify();
            });
        }));
    }
    fn search_members(&mut self, cx: &mut Context<Self>) {
        let term = self.members.search.read(cx).value().trim().to_owned();
        if term.chars().count() < 2 || self.members.searching {
            return;
        }
        self.members.searching = true;
        let query = term.clone();
        let reply = self.core.team_people(self.team.id.clone(), term);
        self.members.search_task = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let result = reply.await;
            let _ = this.update(cx, |this, cx| {
                this.members.searching = false;
                if this.members.search.read(cx).value().trim() == query {
                    this.members.found = result
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|person| {
                            teams_proto::object_id(&person.mri).is_some()
                                && !this.members.listed.iter().any(|member| member.mri == person.mri)
                        })
                        .collect();
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn change_member(&mut self, action: ChannelChange, cx: &mut Context<Self>) {
        if self.core.read_only()
            || self.working.is_some()
            || self.members.loading
            || !channel_menu::can_manage(&self.team, &self.channel)
            || self.team.archived
            || self.channel.about.archived
        {
            return;
        }
        if let ChannelChange::RemoveMember { mri } | ChannelChange::UpdateMember { mri, owner: false } = &action {
            if self
                .members
                .listed
                .iter()
                .find(|member| member.mri == *mri)
                .is_some_and(|member| member.owner)
                && self.members.listed.iter().filter(|member| member.owner).count() <= 1
            {
                self.error = Some("Add another channel owner before removing or demoting the last owner.".into());
                cx.notify();
                return;
            }
        }
        if self.channel.kind != ChannelKind::Standard && self.target().channel_group.is_none() {
            self.error = Some("Refresh the team before changing this channel's members.".into());
            cx.notify();
            return;
        }
        let reply = self.core.team_write(
            TeamWrite::Channel(ChannelWrite {
                target: self.target(),
                action,
            }),
            None,
        );
        self.error = None;
        self.members.remove = None;
        self.working = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let result = reply.await;
            let _ = this.update(cx, |this, cx| {
                this.working = None;
                if result.is_ok() {
                    this.members.found.clear();
                    this.reload_members(cx);
                } else {
                    this.error = Some("The member change wasn't confirmed. Refresh before trying again.".into());
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
    pub(super) fn member_form(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let manage = channel_menu::can_manage(&self.team, &self.channel)
            && !self.core.read_only()
            && !self.team.archived
            && !self.channel.about.archived;
        let private = self.channel.kind != ChannelKind::Standard;
        let mut form = div().flex().flex_col().gap_2();
        if self.members.loading {
            return form.child(team_dialogs::waiting(
                "channel-members-loading",
                "Loading channel members…",
                palette,
            ));
        }
        if private && manage {
            form = form.child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .child(selected_by("channel-member-search", Input::new(&self.members.search))),
                    )
                    .child(selected_by(
                        "channel-member-find",
                        icon_button("channel-member-find", Glyph::SEARCH, "Find people on the team")
                            .on_click(cx.listener(|this, _, _, cx| this.search_members(cx))),
                    )),
            );
            for (index, person) in self.members.found.iter().enumerate().take(10) {
                let mri = person.mri.clone();
                form = form.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .px_2()
                        .py_2()
                        .child(div().flex_1().min_w_0().truncate().child(person.name.clone()))
                        .child(selected_by(
                            format!("channel-member-add-{index}"),
                            icon_button(
                                SharedString::from(format!("channel-member-add-{index}")),
                                Glyph::USER_PLUS,
                                "Add to channel",
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.change_member(
                                    ChannelChange::AddMember {
                                        mri: mri.clone(),
                                        owner: false,
                                    },
                                    cx,
                                )
                            })),
                        )),
                );
            }
        }
        if !private {
            form = form.child(div().text_sm().text_color(rgb(palette.muted_foreground)).child(
                "Everyone on the team has access to this standard channel. Team owners are channel moderators.",
            ));
        }
        for (index, member) in self.members.listed.iter().enumerate() {
            form = form.child(self.member_row(index, member, manage, private, palette, cx));
        }
        form
    }
    fn member_row(
        &self,
        index: usize,
        member: &TeamMember,
        manage: bool,
        private: bool,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label = if member.owner {
            "Owner"
        } else if member.guest {
            "Guest"
        } else {
            "Member"
        };
        let mut row = div()
            .flex()
            .items_center()
            .gap_2()
            .py_2()
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(div().flex_1().min_w_0().truncate().child(member.name.clone()));
        let last_owner = member.owner && self.members.listed.iter().filter(|member| member.owner).count() <= 1;
        if manage && private && !last_owner && !member.guest {
            let mri = member.mri.clone();
            let owner = !member.owner;
            row = row.child(selected_by(
                format!("channel-member-role-{index}"),
                Button::new(SharedString::from(format!("channel-member-role-{index}")))
                    .label(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.change_member(
                            ChannelChange::UpdateMember {
                                mri: mri.clone(),
                                owner,
                            },
                            cx,
                        )
                    })),
            ));
        } else {
            row = row.child(div().text_xs().text_color(rgb(palette.muted_foreground)).child(label));
        }
        if manage && private && !last_owner {
            let mri = member.mri.clone();
            row = row.child(selected_by(
                format!("channel-member-remove-{index}"),
                icon_button(
                    SharedString::from(format!("channel-member-remove-{index}")),
                    Glyph::X,
                    "Remove from channel",
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.members.remove = Some(mri.clone());
                    cx.notify();
                })),
            ));
        } else if manage && !private && !member.owner {
            let mri = member.mri.clone();
            let moderator = !member.moderator;
            row = row.child(selected_by(
                format!("channel-member-moderator-{index}"),
                Button::new(SharedString::from(format!("channel-member-moderator-{index}")))
                    .label(if moderator {
                        "Make moderator"
                    } else {
                        "Remove moderator"
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.change_member(
                            ChannelChange::Moderator {
                                mri: mri.clone(),
                                moderator,
                            },
                            cx,
                        )
                    })),
            ));
        }
        let mut line = div().flex().flex_col().child(row);
        if self.members.remove.as_deref() == Some(&member.mri) {
            let mri = member.mri.clone();
            line = line.child(
                div()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap_2()
                    .child("Remove this person from the channel?")
                    .child(selected_by(
                        "channel-member-remove-cancel",
                        Button::new("channel-member-remove-cancel")
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.members.remove = None;
                                cx.notify();
                            })),
                    ))
                    .child(selected_by(
                        "channel-member-remove-confirm",
                        Button::new("channel-member-remove-confirm")
                            .label("Remove")
                            .danger()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.change_member(ChannelChange::RemoveMember { mri: mri.clone() }, cx)
                            })),
                    )),
            );
        }
        line.into_any_element()
    }
}
