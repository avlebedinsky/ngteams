//! Build from scratch, as browser Teams starts it (`docs/protocol/workflows.md`, "Build from scratch"): an untitled
//! workflow with "Starts when" (Choose what starts this) and "Then do this" (Choose what happens), each choosing from
//! Teams' apps and their triggers or actions. Teams builds the chosen step in Power Automate's designer; here every
//! choice, and "Build with Power Automate", asks Teams' "Build in Power Automate" question and then opens Power
//! Automate's designer for a new workflow in the browser.
use super::*;
use gpui_component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};

/// Teams' trigger apps and the triggers it lists under each (none listed: the app itself is the choice).
const TRIGGERS: [(&str, &[&str]); 5] = [
    (
        "SharePoint",
        &[
            "For a selected file",
            "For a selected list item",
            "A file is created in a library",
            "A list item is created",
            "A file is modified in a library",
            "A list item is modified",
            "A form is submitted",
        ],
    ),
    (
        "Outlook",
        &["A new email arrives", "An event is added, updated, or deleted"],
    ),
    (
        "Teams",
        &[
            "A new message is sent in a channel",
            "A new member is added to a team",
            "Keywords are mentioned in a conversation",
            "A new message is sent in a chat",
            "I'm @mentioned in a chat",
            "Someone reacts in group chat",
            "Someone reacts in channel",
            "From a link",
            "When a webhook request is received",
        ],
    ),
    ("Office 365 Groups", &[]),
    ("Azure DevOps", &[]),
];

/// Teams' action apps and the actions it lists under each.
const ACTIONS: [(&str, &[&str]); 6] = [
    (
        "Teams",
        &[
            "Send a message in a chat or channel",
            "Send a chat message to people",
            "Start a chat",
            "Get notified privately",
            "Post a card to a chat or channel",
            "Notify a person privately",
            "Reply to a message in a channel in Teams",
            "Add a member to a channel",
            "Remove a direct member from a channel",
            "Add a member to a team tag",
            "Create a team tag",
            "Add a member to a team",
            "Remove a member from a team",
            "Remove a member from a team tag",
            "Prompt an agent in a conversation",
            "Prompt an agent to reply in a thread",
            "Archive a channel",
            "Create a channel",
        ],
    ),
    ("Outlook", &[]),
    ("SharePoint", &[]),
    ("Planner", &[]),
    ("Approvals", &[]),
    ("Azure DevOps", &[]),
];

/// Power Automate's designer for a new workflow in `environment`, where Teams' "Go to Power Automate" leads.
pub(super) fn new_flow_url(environment: &str) -> String {
    format!("https://make.powerautomate.com/environments/{environment}/flows/new")
}

impl WorkflowDialog {
    /// Build from scratch: Teams' untitled workflow with nothing chosen yet.
    pub(super) fn start_build(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.core.read_only() {
            return;
        }
        if let Some(app) = &mut self.app {
            app.suggesting = false;
        }
        self.error = None;
        self.screen = Screen::Build { asking: false };
        cx.notify();
    }

    /// Build from scratch, as its button does.
    pub fn build_from_scratch(&mut self, cx: &mut Context<Self>) {
        self.start_build(cx);
    }

    /// Asks "Build in Power Automate", as a choice in the builder does.
    pub fn ask_build_in_power_automate(&mut self, cx: &mut Context<Self>) {
        self.ask_build(true, cx);
    }

    /// Asks, or stops asking, "Build in Power Automate" over the builder.
    fn ask_build(&mut self, asking: bool, cx: &mut Context<Self>) {
        if let Screen::Build { asking: asked } = &mut self.screen {
            *asked = asking;
            cx.notify();
        }
    }

    fn choose_build(&mut self, cx: &mut Context<Self>) {
        self.ask_build(true, cx);
    }

    /// The builder: its title bar, then "Starts when" and "Then do this".
    pub(super) fn build_view(&self, palette: Palette, cx: &Context<Self>) -> AnyElement {
        div()
            .debug_selector(|| "workflow-builder".to_owned())
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(selected_by(
                                "workflow-build-back",
                                icon_button("workflow-build-back", Glyph::CHEVRON_LEFT, "Back")
                                    .ghost()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.screen = this.start_screen();
                                        cx.notify();
                                    })),
                            ))
                            .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child("Untitled workflow")),
                    )
                    .when(self.app.is_some(), |bar| {
                        bar.child(
                            Button::new("workflow-build-save")
                                .primary()
                                .min_w(px(96.0))
                                .label("Save")
                                .disabled(true),
                        )
                    }),
            )
            .child(Self::build_step(
                ("Starts when", "workflow-build-trigger", "Choose what starts this"),
                Glyph::PLAY,
                true,
                palette,
                cx,
            ))
            .child(Self::build_step(
                ("Then do this", "workflow-build-action", "Choose what happens"),
                Glyph::ARROW_RIGHT,
                false,
                palette,
                cx,
            ))
            .into_any_element()
    }

    /// One step of the builder (its heading, selector and words): the heading and the rounded pill that opens Teams'
    /// choices.
    fn build_step(
        (heading, id, label): (&'static str, &'static str, &'static str),
        glyph: Glyph,
        trigger: bool,
        palette: Palette,
        cx: &Context<Self>,
    ) -> AnyElement {
        let weak = cx.entity().downgrade();
        let face = Button::new(id)
            .ghost()
            .w_full()
            .h(px(50.0))
            .rounded_full()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.background))
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .size(px(24.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .bg(rgb(palette.hover))
                            .child(glyph.icon().size(px(12.0))),
                    )
                    .child(div().flex_1().child(label))
                    .child(
                        Glyph::PLUS
                            .icon()
                            .size(px(14.0))
                            .text_color(rgb(palette.muted_foreground)),
                    ),
            )
            .dropdown_menu(move |menu, window, cx| Self::build_menu(menu, trigger, &weak, window, cx));
        div()
            .flex()
            .flex_col()
            .gap_2()
            .pt_6()
            .child(
                div()
                    .px_4()
                    .text_lg()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(heading),
            )
            .child(selected_by(id, face))
            .into_any_element()
    }

    /// Teams' choices of a step: On a schedule (a trigger), each app and what it offers, then Power Automate.
    fn build_menu(
        mut menu: PopupMenu,
        trigger: bool,
        weak: &WeakEntity<Self>,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let choose = |weak: WeakEntity<Self>| {
            move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| {
                let _ = weak.update(cx, WorkflowDialog::choose_build);
            }
        };
        let row = |key: String, label: &'static str| {
            PopupMenuItem::element(move |_, _| {
                let key = key.clone();
                div().debug_selector(move || key.clone()).child(label)
            })
        };
        if trigger {
            menu = menu.item(row("workflow-build-schedule".to_owned(), "On a schedule").on_click(choose(weak.clone())));
        }
        for (app, choices) in if trigger { &TRIGGERS[..] } else { &ACTIONS[..] } {
            if choices.is_empty() {
                menu = menu.item(row(format!("workflow-build-app-{app}"), app).on_click(choose(weak.clone())));
                continue;
            }
            let weak = weak.clone();
            menu = menu.submenu(*app, window, cx, move |mut submenu, _, _| {
                for choice in *choices {
                    submenu = submenu
                        .item(PopupMenuItem::element(move |_, _| div().child(*choice)).on_click(choose(weak.clone())));
                }
                submenu
            });
        }
        menu.separator().item(
            row(
                "workflow-build-power-automate".to_owned(),
                if trigger {
                    "Build with Power Automate to see more triggers"
                } else {
                    "Build with Power Automate to see more actions"
                },
            )
            .on_click(choose(weak.clone())),
        )
    }

    /// Teams' "Build in Power Automate" question.
    pub(super) fn build_prompt(&self, palette: Palette, cx: &Context<Self>) -> AnyElement {
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::rgba(0x0000_0066))
            .occlude()
            .child(
                div()
                    .debug_selector(|| "workflow-build-ask".to_owned())
                    .w(px(600.0))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .p_6()
                    .rounded_lg()
                    .shadow_lg()
                    .bg(rgb(palette.surface))
                    .child(
                        div()
                            .text_xl()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Build in Power Automate"),
                    )
                    .child(
                        "You'll be taken to Power Automate to see the full catalog of triggers and actions you can \
                         use to build your workflow. After your workflow is created, you'll see it in your list of \
                         workflows, but it will only be editable in Power Automate.",
                    )
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(selected_by(
                                "workflow-build-go",
                                Button::new("workflow-build-go")
                                    .primary()
                                    .label("Go to Power Automate")
                                    .disabled(self.environment.is_none())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if let Some(environment) = &this.environment {
                                            cx.open_url(&new_flow_url(environment));
                                        }
                                        this.ask_build(false, cx);
                                    })),
                            ))
                            .child(selected_by(
                                "workflow-build-cancel",
                                Button::new("workflow-build-cancel")
                                    .outline()
                                    .min_w(px(96.0))
                                    .label("Cancel")
                                    .on_click(cx.listener(|this, _, _, cx| this.ask_build(false, cx))),
                            )),
                    ),
            )
            .into_any_element()
    }
}
