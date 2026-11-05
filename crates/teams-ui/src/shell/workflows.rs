//! The Workflows app's places in the shell, as browser Teams has them: opened from the channel's Workflows ("Workflows
//! Home", "See all") or from the rail's More apps flyout, it takes both panes; while it is shown, the rail shows it
//! (chosen) under a separator, and it leaves the rail when another place is shown.
use super::*;
use crate::workflows_app::WorkflowsApp;

/// The rail's Workflows item: its outline glyph (Lucide's) and its caption.
const WORKFLOWS: RailItem = RailItem {
    icon: include_bytes!("../../assets/icons/workflow.svg"),
    label: "Workflows",
};

/// The rail's More apps item.
const MORE_APPS: RailItem = RailItem {
    icon: include_bytes!("../../assets/icons/ellipsis.svg"),
    label: "More apps",
};

/// The flyout's width, px.
const FLYOUT_WIDTH: f32 = 300.0;

/// The Workflows app's place in the shell: the app once it was opened (it keeps its tab and pages while another place
/// is shown), and whether the rail's More apps flyout is open.
#[derive(Default)]
pub(super) struct Place {
    pub(super) app: Option<Entity<WorkflowsApp>>,
    pub(super) more_apps: bool,
}

impl Shell {
    /// Opens the Workflows app across both panes on its Home, or with `see_all` its "Your workflows", as the channel's
    /// Workflows asks: its list is read again, as changes made there may have left it behind.
    pub fn open_workflows(&mut self, see_all: bool, window: &mut Window, cx: &mut Context<Self>) {
        let page = self.enter_workflows(window, cx);
        page.update(cx, |page, cx| {
            if see_all {
                page.show_all_workflows(window, cx);
            } else {
                page.show_home(window, cx);
            }
            page.reread_workflows(cx);
        });
        cx.notify();
    }

    /// Shows the Workflows app again as it was left (the rail's item, More apps, history), or on its Home the first
    /// time.
    pub fn resume_workflows(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.workflows.app.is_none() {
            self.open_workflows(false, window, cx);
            return;
        }
        let page = self.enter_workflows(window, cx);
        page.update(cx, |page, cx| page.resume(window, cx));
        cx.notify();
    }

    /// The app, made the first time, across both panes.
    fn enter_workflows(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<WorkflowsApp> {
        self.workflows.more_apps = false;
        let page = if let Some(page) = &self.workflows.app {
            page.clone()
        } else {
            let parts = self.parts();
            let page = cx.new(|cx| WorkflowsApp::new(parts, window, cx));
            cx.observe(&page, |_, _, cx| cx.notify()).detach();
            self.workflows.app = Some(page.clone());
            page
        };
        if self.main != Main::Workflows {
            self.trace.record(&TraceLine::new("workflows", "opened"));
            self.hide_calendar(window, cx);
            let before = std::mem::replace(&mut self.main, Main::Workflows);
            if !matches!(before, Main::Settings | Main::Calendar | Main::Search) {
                self.before_settings = before;
            }
            self.put_chat_away(cx);
            self.close_switcher(window, cx);
            self.close_search(cx);
        }
        page
    }

    /// Runs a workflow on a message, as Teams' message menu does: its run dialog over the window, asking the questions
    /// the workflow's trigger asks.
    pub fn run_workflow(
        &mut self,
        flow: crate::workflow_dialog::FlowRef,
        message: crate::workflow_run::MessageRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.core.read_only() {
            return;
        }
        let message = self.with_team(message, cx);
        let core = Arc::clone(&self.core);
        let dialog = cx.new(|cx| crate::workflow_run::WorkflowRun::new(core, flow, message, window, cx));
        self.show_team_dialog(&dialog, window, cx);
    }

    /// A message's "More actions" → "Create workflows": the Workflows dialog for that message, its templates run from a
    /// message first and its list the workflows that run on it. A channel message's channel is where a channel
    /// template starts.
    pub fn open_message_workflows(
        &mut self,
        message: crate::workflow_run::MessageRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let message = self.with_team(message, cx);
        let place = self.channel_place(&message, cx);
        let core = Arc::clone(&self.core);
        let dialog =
            cx.new(|cx| crate::workflow_dialog::WorkflowDialog::new_for_message(core, place, message, window, cx));
        self.show_team_dialog(&dialog, window, cx);
    }

    /// A channel message with its team's thread and group, as the teams list holds them.
    fn with_team(&self, mut message: crate::workflow_run::MessageRef, cx: &App) -> crate::workflow_run::MessageRef {
        if message.in_channel && message.team.is_none() {
            message.team = self.channel_place(&message, cx).map(|(team, _)| {
                let group = team.group_id.clone().unwrap_or_default();
                (team.id, group)
            });
        }
        message
    }

    /// The team and channel a channel message is in, as the teams list holds them.
    fn channel_place(
        &self,
        message: &crate::workflow_run::MessageRef,
        cx: &App,
    ) -> Option<(teams_store::Team, teams_store::TeamChannel)> {
        if !message.in_channel {
            return None;
        }
        let teams = self.teams.read(cx).teams()?;
        teams.iter().find_map(|team| {
            let channel = team.channels.iter().find(|known| known.id == message.conversation)?;
            Some((team.clone(), channel.clone()))
        })
    }

    /// Opens or closes the rail's More apps flyout, as its button does.
    pub fn toggle_more_apps(&mut self, cx: &mut Context<Self>) {
        self.workflows.more_apps = !self.workflows.more_apps;
        cx.notify();
    }

    /// The Workflows app, once it was opened.
    #[must_use]
    pub fn workflows(&self) -> Option<&Entity<WorkflowsApp>> {
        self.workflows.app.as_ref()
    }

    /// The Workflows app is left for another place: its bot's chat is no longer shown.
    pub(super) fn leave_workflows(&mut self, cx: &mut Context<Self>) {
        if let Some(page) = &self.workflows.app {
            page.update(cx, WorkflowsApp::hide);
        }
    }

    /// The rail's More apps button and, while the app is shown, the Workflows item above it.
    pub(super) fn app_rail(&self, palette: Palette, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut items = Vec::new();
        if self.main == Main::Workflows {
            items.push(
                div()
                    .mx_3()
                    .my_1()
                    .h(px(1.0))
                    .bg(rgb(palette.on_frame).opacity(0.25))
                    .into_any_element(),
            );
            items.push(Self::rail_button(
                "rail-workflows",
                WORKFLOWS,
                true,
                palette,
                cx,
                Self::resume_workflows,
            ));
        }
        items.push(Self::rail_button(
            "rail-more-apps",
            MORE_APPS,
            self.workflows.more_apps,
            palette,
            cx,
            |this, _, cx| this.toggle_more_apps(cx),
        ));
        items
    }

    /// The More apps flyout beside the rail: the apps the app opens (Workflows), over a backdrop that closes it.
    pub(super) fn more_apps_view(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.workflows.more_apps {
            return None;
        }
        let row = div()
            .id("more-apps-workflows")
            .debug_selector(|| "more-apps-workflows".to_owned())
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .py_2()
            .rounded_md()
            .cursor_pointer()
            .hover(move |row| row.bg(rgb(palette.hover)))
            .child(
                div()
                    .size(px(26.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(0x00ff_ffff))
                    .child(gpui::img(crate::workflow_dialog::workflows_icon()).size(px(18.0))),
            )
            .child("Workflows")
            .on_click(cx.listener(|this, _, window, cx| this.resume_workflows(window, cx)));
        Some(
            div()
                .id("more-apps-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.workflows.more_apps = false;
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .id("more-apps")
                        .debug_selector(|| "more-apps".to_owned())
                        .absolute()
                        .top(px(TITLE_HEIGHT + 8.0))
                        .left(px(RAIL_WIDTH + 4.0))
                        .w(px(FLYOUT_WIDTH))
                        .p_2()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .rounded_lg()
                        .border_1()
                        .border_color(rgb(palette.border))
                        .bg(rgb(palette.surface))
                        .shadow_lg()
                        .occlude()
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(
                            div()
                                .px_3()
                                .py_1()
                                .text_sm()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(rgb(palette.muted_foreground))
                                .child("Apps"),
                        )
                        .child(row),
                )
                .into_any_element(),
        )
    }
}
