//! "More create team options", which "Create a team" links to, as Teams has it (`docs/protocol/teams.md`): a page across
//! both panes with Back and, in its own column, From template, From another team and From group.
//!
//! - **From template**: "From scratch" (the "Create a team" dialog again) and a card per template Teams offers
//!   ([`Core::team_templates`]); a card shows the template's channels and apps ([`Core::team_template`]), and "Use this
//!   template" leads to its form: name, description and privacy, then Create.
//! - **From another team**: a card per team the account owns; one picked leads to its form, the name and description
//!   filled from it, the privacy, and what comes along (channels and apps always; tabs, settings and its people when
//!   ticked).
//! - **From group**: the Microsoft 365 groups the account owns ([`Core::owned_groups`]); with none, Teams' words for it.
//!
//! Nothing here sends anything but through [`Core`], and only on Create. The read-only mode shows the cards and offers no
//! Create.

use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, AsyncApp, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task,
    WeakEntity, Window, div, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Disableable as _, Sizable as _};
use teams_proto::{TEMPLATE_LOCALE, TeamCopy, TeamTemplateCard, TeamTemplateDetails, TeamWrite, TemplateTeam};
use teams_store::Team;

use crate::bridge::Core;
use crate::icons::{Glyph, icon_button, selected_by};
use crate::keyboard;
use crate::team_dialogs::{team_description_problem, team_name_problem};
use crate::teams_list::initials;
use crate::teams_page::centered;
use crate::theme::{self, Palette};

/// Where a new team comes from: the page's own column.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MoreSource {
    #[default]
    Template,
    AnotherTeam,
    Group,
}

impl MoreSource {
    const ALL: [Self; 3] = [Self::Template, Self::AnotherTeam, Self::Group];

    fn key(self) -> &'static str {
        match self {
            Self::Template => "template",
            Self::AnotherTeam => "another-team",
            Self::Group => "group",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Template => "From template",
            Self::AnotherTeam => "From another team",
            Self::Group => "From group",
        }
    }
}

/// What the read of the account's groups found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GroupsRead {
    Unread,
    Count(usize),
    /// The read failed, or answered with no list.
    Unknown,
}

/// What the page tells the shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CreateTeamMoreEvent {
    Back,
    /// "From scratch": the "Create a team" dialog.
    FromScratch,
    /// A team made; the words go in a toast.
    Created(&'static str),
}

/// What a form makes a team of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MoreChoice {
    /// A template, its channels and apps once read.
    Template(TeamTemplateCard, Option<TeamTemplateDetails>),
    /// Another team.
    Team(Team),
}

/// What a copy brings along beside its channels and apps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "three boxes the user ticks, each on its own, as Teams' dialog has them"
)]
pub struct CopyParts {
    pub tabs: bool,
    pub settings: bool,
    pub people: bool,
    /// Whether the form is shown yet (a template's card shows its details first).
    form: bool,
}

/// "More create team options" (see the module docs).
pub struct CreateTeamMore {
    core: Arc<dyn Core>,
    owned: Vec<Team>,
    source: MoreSource,
    templates: Option<Vec<TeamTemplateCard>>,
    groups: GroupsRead,
    choice: Option<MoreChoice>,
    name: Entity<InputState>,
    description: Entity<InputState>,
    public: bool,
    parts: CopyParts,
    sending: bool,
    problem: Option<&'static str>,
    reading: Option<Task<()>>,
    writing: Option<Task<()>>,
    _inputs: [Subscription; 2],
}

impl EventEmitter<CreateTeamMoreEvent> for CreateTeamMore {}

impl CreateTeamMore {
    /// The page on "From template", the templates being read; `owned` are the teams the account owns.
    pub fn new(core: Arc<dyn Core>, owned: Vec<Team>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Give your team a name"));
        let description =
            cx.new(|cx| InputState::new(window, cx).placeholder("Let people know what this team is all about"));
        let typed = cx.subscribe(&name, |_, _, _: &InputEvent, cx| cx.notify());
        let described = cx.subscribe(&description, |_, _, _: &InputEvent, cx| cx.notify());
        let mut page = Self {
            core,
            owned,
            source: MoreSource::Template,
            templates: None,
            groups: GroupsRead::Unread,
            choice: None,
            name,
            description,
            public: false,
            parts: CopyParts {
                tabs: true,
                settings: true,
                people: false,
                form: false,
            },
            sending: false,
            problem: None,
            reading: None,
            writing: None,
            _inputs: [typed, described],
        };
        page.show(MoreSource::Template, cx);
        page
    }

    /// The source shown.
    #[must_use]
    pub fn source(&self) -> MoreSource {
        self.source
    }

    /// The templates read, none before the read.
    #[must_use]
    pub fn templates(&self) -> &[TeamTemplateCard] {
        self.templates.as_deref().unwrap_or_default()
    }

    /// The teams the account owns, which "From another team" offers.
    #[must_use]
    pub fn owned(&self) -> &[Team] {
        &self.owned
    }

    /// The choice a form is open on.
    #[must_use]
    pub fn choice(&self) -> Option<&MoreChoice> {
        self.choice.as_ref()
    }

    /// Whether the form shows (not a template's details).
    #[must_use]
    pub fn form_shown(&self) -> bool {
        self.choice.is_some() && self.parts.form
    }

    /// Shows `source`, read now when it reads anything.
    pub fn show(&mut self, source: MoreSource, cx: &mut Context<Self>) {
        self.source = source;
        self.choice = None;
        self.problem = None;
        let core = Arc::clone(&self.core);
        match source {
            MoreSource::Template if self.templates.is_none() => {
                self.reading = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                    let templates = core.team_templates().await.unwrap_or_default();
                    let _ = this.update(cx, |this, cx| {
                        this.templates = Some(templates);
                        cx.notify();
                    });
                }));
            }
            MoreSource::Group if self.groups == GroupsRead::Unread => {
                self.reading = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                    let groups = core
                        .owned_groups()
                        .await
                        .ok()
                        .flatten()
                        .map_or(GroupsRead::Unknown, GroupsRead::Count);
                    let _ = this.update(cx, |this, cx| {
                        this.groups = groups;
                        cx.notify();
                    });
                }));
            }
            _ => {}
        }
        cx.notify();
    }

    /// A template's card: its details, read now.
    pub fn pick_template(&mut self, card: TeamTemplateCard, cx: &mut Context<Self>) {
        let id = card.id.clone();
        self.choice = Some(MoreChoice::Template(card, None));
        self.parts.form = false;
        self.problem = None;
        let reply = self.core.team_template(id.clone());
        self.reading = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let details = reply.await.ok().flatten();
            let _ = this.update(cx, |this, cx| {
                if let Some(MoreChoice::Template(card, read)) = &mut this.choice
                    && card.id == id
                {
                    *read = details;
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// "Use this template": the template's form.
    pub fn use_template(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.fill("", "", window, cx);
        self.public = false;
        self.parts.form = true;
        cx.notify();
    }

    /// A team the account owns: its form, filled from it.
    pub fn pick_team(&mut self, team: Team, window: &mut Window, cx: &mut Context<Self>) {
        self.fill(&format!("{} [copy]", team.name), &team.about.description, window, cx);
        self.public = team.about.public;
        self.parts = CopyParts {
            tabs: true,
            settings: true,
            people: false,
            form: true,
        };
        self.problem = None;
        self.choice = Some(MoreChoice::Team(team));
        cx.notify();
    }

    /// The form's name and description.
    pub fn fill(&mut self, name: &str, description: &str, window: &mut Window, cx: &mut Context<Self>) {
        let (name, description) = (name.to_owned(), description.to_owned());
        self.name.update(cx, |input, cx| input.set_value(name, window, cx));
        self.description
            .update(cx, |input, cx| input.set_value(description, window, cx));
    }

    /// The form's privacy.
    pub fn set_public(&mut self, public: bool, cx: &mut Context<Self>) {
        self.public = public;
        cx.notify();
    }

    /// A copy's boxes: tabs, settings, people.
    pub fn set_parts(&mut self, tabs: bool, settings: bool, people: bool, cx: &mut Context<Self>) {
        self.parts.tabs = tabs;
        self.parts.settings = settings;
        self.parts.people = people;
        cx.notify();
    }

    /// Closes the form or a template's details.
    pub fn close_choice(&mut self, cx: &mut Context<Self>) {
        self.choice = None;
        self.parts.form = false;
        self.problem = None;
        cx.notify();
    }

    fn values(&self, cx: &gpui::App) -> (String, String) {
        (
            self.name.read(cx).value().trim().to_owned(),
            self.description.read(cx).value().trim().to_owned(),
        )
    }

    /// Whether Create is on: a name that reads, a description that reads, a template's channels read.
    #[must_use]
    pub fn can_create(&self, cx: &gpui::App) -> bool {
        let (name, description) = self.values(cx);
        let read = match &self.choice {
            Some(MoreChoice::Template(_, details)) => details.is_some(),
            Some(MoreChoice::Team(_)) => true,
            None => false,
        };
        read && !self.sending
            && !name.is_empty()
            && team_name_problem(&name).is_none()
            && team_description_problem(&description).is_none()
    }

    /// The write Create sends.
    #[must_use]
    pub fn write(&self, cx: &gpui::App) -> Option<TeamWrite> {
        let (name, description) = self.values(cx);
        match self.choice.as_ref()? {
            MoreChoice::Template(card, details) => {
                let details = details.as_ref()?;
                Some(TeamWrite::FromTemplate(TemplateTeam {
                    template: card.id.clone(),
                    name,
                    description,
                    public: self.public,
                    locale: TEMPLATE_LOCALE.to_owned(),
                    channels: details
                        .channels
                        .iter()
                        .map(|channel| (channel.id.clone(), channel.name.clone()))
                        .collect(),
                    apps: details.apps.clone(),
                }))
            }
            MoreChoice::Team(team) => Some(TeamWrite::Copy(TeamCopy {
                group: team.group_id.clone()?,
                name,
                description,
                public: self.public,
                owner: self.core.me().trim_start_matches("8:orgid:").to_owned(),
                roster: self.parts.people,
                channels: true,
                apps: true,
                settings: self.parts.settings,
                tabs: self.parts.tabs,
            })),
        }
    }

    /// Create.
    pub fn create(&mut self, cx: &mut Context<Self>) {
        if !self.can_create(cx) {
            return;
        }
        let Some(write) = self.write(cx) else {
            return;
        };
        self.sending = true;
        self.problem = None;
        let reply = self.core.team_write(write, None);
        self.writing = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let answer = reply.await;
            let _ = this.update(cx, |this, cx| {
                this.sending = false;
                if answer.is_ok() {
                    cx.emit(CreateTeamMoreEvent::Created("Your team was created"));
                } else {
                    this.problem = Some("We ran into a problem creating your team. Please try again later.");
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
}

/// A card of the page's grid: a badge, a name, a line under it and a description.
fn card(
    id: String,
    badge: String,
    name: &str,
    under: &str,
    about: &str,
    palette: Palette,
) -> gpui::Stateful<gpui::Div> {
    let selector = id.clone();
    div()
        .debug_selector(move || selector.clone())
        .id(SharedString::from(id))
        .w(px(260.0))
        .h(px(120.0))
        .p_3()
        .flex()
        .flex_col()
        .gap_2()
        .rounded_lg()
        .border_1()
        .border_color(rgb(palette.border))
        .bg(rgb(palette.surface))
        .cursor_pointer()
        .hover(move |card| card.bg(rgb(palette.hover)))
        .child(
            div().flex().gap_2().child(badge_tile(badge, 40.0, palette)).child(
                div()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .truncate()
                            .child(name.to_owned()),
                    )
                    .child(div().text_sm().truncate().child(under.to_owned())),
            ),
        )
        .child(
            div()
                .text_sm()
                .overflow_hidden()
                .text_color(rgb(palette.muted_foreground))
                .child(about.to_owned()),
        )
}

/// A square of the brand colour with letters on it: a template's or a team's face.
pub(crate) fn badge_tile(face: String, size: f32, palette: Palette) -> gpui::Div {
    div()
        .size(px(size))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .bg(rgb(palette.brand))
        .text_color(rgb(palette.on_brand))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .child(face)
}

impl CreateTeamMore {
    fn side_panel(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let source = self.source;
        div()
            .w(px(320.0))
            .flex_none()
            .h_full()
            .p_3()
            .flex()
            .flex_col()
            .gap_1()
            .border_r_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .pb_2()
                    .child(selected_by(
                        "create-more-back",
                        icon_button("create-more-back", Glyph::CHEVRON_LEFT, "Back")
                            .small()
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(CreateTeamMoreEvent::Back))),
                    ))
                    .child(div().text_lg().font_weight(gpui::FontWeight::BOLD).child("Create team")),
            )
            .children(MoreSource::ALL.into_iter().map(|entry| {
                let id = format!("create-more-{}", entry.key());
                keyboard::stop(
                    id.clone(),
                    div()
                        .id(SharedString::from(id.clone()))
                        .debug_selector(move || id.clone())
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .cursor_pointer()
                        .when(entry == source, |row| row.bg(rgb(palette.selected)))
                        .hover(move |row| row.bg(rgb(palette.hover)))
                        .on_click(cx.listener(move |this, _, _, cx| this.show(entry, cx)))
                        .child(entry.label()),
                    palette,
                )
            }))
    }

    fn templates_view(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let scratch = keyboard::stop(
            "create-more-scratch",
            card(
                "create-more-scratch".to_owned(),
                "+".to_owned(),
                "From scratch",
                "",
                "We'll help you create a basic team.",
                palette,
            )
            .on_click(cx.listener(|_, _, _, cx| cx.emit(CreateTeamMoreEvent::FromScratch))),
            palette,
        )
        .edge();
        let cards: Vec<AnyElement> = self
            .templates
            .clone()
            .unwrap_or_default()
            .into_iter()
            .enumerate()
            .map(|(at, template)| {
                let id = format!("create-more-template-{at}");
                let picked = template.clone();
                keyboard::stop(
                    id.clone(),
                    card(
                        id,
                        initials(&template.name),
                        &template.name,
                        &template.category,
                        &template.summary,
                        palette,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.pick_template(picked.clone(), cx))),
                    palette,
                )
                .edge()
                .into_any_element()
            })
            .collect();
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(heading(
                "Create a team from a template",
                "Skip the setup and use a template to get your team engaged quickly.",
                palette,
            ))
            .when(self.templates.is_none(), |view| {
                view.child(centered("Loading…", palette))
            })
            .child(div().flex().flex_wrap().gap_4().child(scratch).children(cards))
            .into_any_element()
    }

    fn teams_view(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let cards: Vec<AnyElement> = self
            .owned
            .iter()
            .enumerate()
            .map(|(at, team)| {
                let id = format!("create-more-team-{at}");
                let picked = team.clone();
                keyboard::stop(
                    id.clone(),
                    card(
                        id,
                        initials(&team.name),
                        &team.name,
                        if team.about.public { "Public" } else { "Private" },
                        &team.about.description,
                        palette,
                    )
                    .on_click(cx.listener(move |this, _, window, cx| this.pick_team(picked.clone(), window, cx))),
                    palette,
                )
                .edge()
                .into_any_element()
            })
            .collect();
        let none = self.owned.is_empty();
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(heading(
                "Which team do you want to use?",
                "Copy apps, settings, and channels over to your new team. Your existing team won't be changed",
                palette,
            ))
            .when(none, |view| {
                view.child(empty_note(
                    "create-more-no-team",
                    "You need to own a team to create another team from it.",
                    None,
                    palette,
                ))
            })
            .child(div().flex().flex_wrap().gap_4().children(cards))
            .into_any_element()
    }

    fn groups_view(&self, palette: Palette) -> AnyElement {
        let body: AnyElement = match self.groups {
            GroupsRead::Unread => centered("Loading…", palette),
            GroupsRead::Count(0) | GroupsRead::Unknown => empty_note(
                "create-more-no-group",
                "You need to own a group with fewer than 10000 people to create a team from that group.",
                Some("Please create a group first."),
                palette,
            )
            .into_any_element(),
            // no capture has shown a group's entry, so only how many there are is read
            GroupsRead::Count(count) => empty_note(
                "create-more-groups",
                if count == 1 {
                    "You own 1 group."
                } else {
                    "You own some groups."
                },
                Some("Making a team of one of them is done in Teams on the web for now."),
                palette,
            )
            .into_any_element(),
        };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(heading(
                "Which Microsoft 365 group do you want to use?",
                "This will permanently add Teams to your existing Microsoft 365 Group.",
                palette,
            ))
            .child(body)
            .into_any_element()
    }
}

fn heading(title: &'static str, about: &'static str, palette: Palette) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_lg().font_weight(gpui::FontWeight::BOLD).child(title))
        .child(div().text_sm().text_color(rgb(palette.muted_foreground)).child(about))
}

fn empty_note(selector: &'static str, words: &'static str, more: Option<&'static str>, palette: Palette) -> gpui::Div {
    div()
        .debug_selector(move || selector.to_owned())
        .pt_16()
        .flex()
        .flex_col()
        .items_center()
        .gap_3()
        .child(
            div()
                .size(px(72.0))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgb(palette.selected))
                .text_color(rgb(palette.brand))
                .child(Glyph::USERS.icon().size(px(32.0))),
        )
        .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(words))
        .children(more.map(|more| div().text_sm().text_color(rgb(palette.muted_foreground)).child(more)))
}

impl CreateTeamMore {
    fn details(
        &self,
        card: &TeamTemplateCard,
        details: Option<&TeamTemplateDetails>,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let channels: Vec<gpui::Div> = details
            .map(|details| {
                details
                    .channels
                    .iter()
                    .map(|channel| div().text_sm().child(channel.name.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let (channel_count, app_count) = details.map_or((0, 0), |details| (details.channels.len(), details.apps.len()));
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_xl()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(card.name.clone()),
            )
            .child(div().text_sm().child(card.description.clone()))
            .when(details.is_none(), |view| view.child(centered("Loading…", palette)))
            .when(details.is_some(), |view| {
                view.child(
                    div()
                        .flex()
                        .gap_8()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child(counted(channel_count, "channel")),
                                )
                                .children(channels),
                        )
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(counted(app_count, "app")),
                        ),
                )
            })
            .when(!self.core.read_only(), |view| {
                view.child(
                    div().flex().justify_end().child(selected_by(
                        "create-more-use-template",
                        Button::new("create-more-use-template")
                            .primary()
                            .label("Use this template")
                            .disabled(details.is_none())
                            .on_click(cx.listener(|this, _, window, cx| this.use_template(window, cx))),
                    )),
                )
            })
    }

    fn privacy_chip(&self, public: bool, palette: Palette, cx: &mut Context<Self>) -> keyboard::Stop {
        let id = if public {
            "create-more-public"
        } else {
            "create-more-private"
        };
        let chosen = self.public == public;
        keyboard::stop(
            id,
            div()
                .id(id)
                .debug_selector(move || id.to_owned())
                .h_8()
                .px_3()
                .flex()
                .items_center()
                .gap_2()
                .rounded_md()
                .border_1()
                .border_color(rgb(if chosen { palette.brand } else { palette.border }))
                .when(chosen, |chip| chip.bg(rgb(palette.selected)))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| this.set_public(public, cx)))
                .child(if public { Glyph::GLOBE } else { Glyph::LOCK }.icon().size(px(16.0)))
                .child(if public { "Public" } else { "Private" }),
            palette,
        )
        .edge()
    }

    fn copy_boxes(&self, team: &Team, cx: &mut Context<Self>) -> gpui::Div {
        let parts = self.parts;
        let tick = |id: &'static str, words: String, on: bool, fixed: bool, set: fn(&mut CopyParts, bool)| {
            selected_by(
                id,
                Checkbox::new(id)
                    .label(words)
                    .checked(on)
                    .disabled(fixed)
                    .on_click(cx.listener(move |this, on: &bool, _, cx| {
                        let mut parts = this.parts;
                        set(&mut parts, *on);
                        this.set_parts(parts.tabs, parts.settings, parts.people, cx);
                    })),
            )
        };
        let people = if team.about.people == 1 {
            "Member (1 person)".to_owned()
        } else {
            format!("Members ({} people)", team.about.people)
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Choose what you'd like to include from the original team"),
            )
            .child(
                div()
                    .grid()
                    .grid_cols(2)
                    .gap_2()
                    .child(tick(
                        "create-more-copy-channels",
                        "Channels".to_owned(),
                        true,
                        true,
                        |_, _| {},
                    ))
                    .child(tick("create-more-copy-apps", "Apps".to_owned(), true, true, |_, _| {}))
                    .child(tick(
                        "create-more-copy-tabs",
                        "Tabs".to_owned(),
                        parts.tabs,
                        false,
                        |parts, on| parts.tabs = on,
                    ))
                    .child(tick(
                        "create-more-copy-settings",
                        "Team settings".to_owned(),
                        parts.settings,
                        false,
                        |parts, on| parts.settings = on,
                    ))
                    .child(tick(
                        "create-more-copy-people",
                        people,
                        parts.people,
                        false,
                        |parts, on| parts.people = on,
                    )),
            )
    }

    fn form(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let (name, description) = self.values(cx);
        let can_create = self.can_create(cx);
        let title = match &self.choice {
            Some(MoreChoice::Team(team)) => format!("You are using \"{}\" as a template for a new team", team.name),
            _ => "Create a team".to_owned(),
        };
        let copied = match &self.choice {
            Some(MoreChoice::Team(team)) => Some(team.clone()),
            _ => None,
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().text_xl().font_weight(gpui::FontWeight::BOLD).child(title))
            .child(field_label("Team name", true, palette))
            .child(selected_by("create-more-name", Input::new(&self.name)))
            .children(team_name_problem(&name).map(|words| problem("create-more-name-problem", words)))
            .child(field_label("Description", false, palette))
            .child(selected_by("create-more-description", Input::new(&self.description)))
            .children(
                team_description_problem(&description).map(|words| problem("create-more-description-problem", words)),
            )
            .child(field_label(
                if copied.is_some() { "Privacy" } else { "Team type" },
                false,
                palette,
            ))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(self.privacy_chip(false, palette, cx))
                    .child(self.privacy_chip(true, palette, cx)),
            )
            .children(copied.map(|team| self.copy_boxes(&team, cx)))
            .children(self.problem.map(|words| problem("create-more-problem", words)))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .pt_2()
                    .child(selected_by(
                        "create-more-cancel",
                        Button::new("create-more-cancel")
                            .outline()
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| this.close_choice(cx))),
                    ))
                    .when(!self.core.read_only(), |row| {
                        row.child(selected_by(
                            "create-more-create",
                            Button::new("create-more-create")
                                .primary()
                                .label(if self.sending { "Creating…" } else { "Create" })
                                .disabled(!can_create)
                                .on_click(cx.listener(|this, _, _, cx| this.create(cx))),
                        ))
                    }),
            )
    }

    /// The dialog over the page: a template's details or a form.
    fn chosen_view(
        &self,
        window: &Window,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Stateful<gpui::Div>> {
        let choice = self.choice.clone()?;
        let inner = match (&choice, self.parts.form) {
            (MoreChoice::Template(card, details), false) => self.details(card, details.as_ref(), palette, cx),
            _ => self.form(palette, cx),
        };
        Some(
            div()
                .id("create-more-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .occlude()
                .bg(gpui::rgba(0x0000_0080))
                .child(
                    div()
                        .debug_selector(|| "create-more-dialog".to_owned())
                        .id("create-more-dialog")
                        .relative()
                        .w(px(600.0))
                        .max_w_full()
                        .max_h(window.viewport_size().height - px(48.0))
                        .overflow_y_scroll()
                        .p_6()
                        .rounded_lg()
                        .border_1()
                        .border_color(rgb(palette.border))
                        .bg(rgb(palette.surface))
                        .shadow_lg()
                        .child(
                            div().absolute().top_3().right_3().child(selected_by(
                                "create-more-close",
                                icon_button("create-more-close", Glyph::X, "Close")
                                    .small()
                                    .on_click(cx.listener(|this, _, _, cx| this.close_choice(cx))),
                            )),
                        )
                        .child(inner),
                ),
        )
    }
}

/// `count` of `thing`, plural but for one.
fn counted(count: usize, thing: &str) -> String {
    if count == 1 {
        format!("1 {thing}")
    } else {
        format!("{count} {thing}s")
    }
}

fn field_label(words: &'static str, required: bool, palette: Palette) -> gpui::Div {
    div()
        .flex()
        .gap_1()
        .text_sm()
        .text_color(rgb(palette.foreground))
        .child(words)
        .when(required, |label| {
            label.child(div().text_color(rgb(theme::DANGER)).child("*"))
        })
}

fn problem(selector: &'static str, words: &'static str) -> gpui::Div {
    div()
        .debug_selector(move || selector.to_owned())
        .text_xs()
        .text_color(rgb(theme::DANGER))
        .child(words)
}

impl Render for CreateTeamMore {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let content = match self.source {
            MoreSource::Template => self.templates_view(palette, cx),
            MoreSource::AnotherTeam => self.teams_view(palette, cx),
            MoreSource::Group => self.groups_view(palette),
        };
        let chosen = self.chosen_view(window, palette, cx);
        div()
            .debug_selector(|| "create-more".to_owned())
            .relative()
            .size_full()
            .flex()
            .bg(rgb(palette.background))
            .text_color(rgb(palette.foreground))
            .child(self.side_panel(palette, cx))
            .child(
                div()
                    .id("create-more-content")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .p_5()
                    .overflow_y_scroll()
                    .child(content),
            )
            .children(chosen)
    }
}
