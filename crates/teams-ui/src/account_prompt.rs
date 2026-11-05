//! The personal build's first sign-in screen (`docs/personal-build.md`, "How the implementation reads it"): the window
//! `ngteams` opens when no account is signed in yet, asking for the work account as Teams' own sign-in does, with "Sign
//! in". Enter or the button asks the app to sign that account in ([`AccountPromptEvent::SignIn`]); while Microsoft's
//! sign-in window is open the field and the button wait, and a sign-in that stored nothing comes back as a line under
//! the field ([`AccountPrompt::failed`]), the field open again. It sends nothing itself: the account goes only to the
//! app, which checks it and hands it to the sign-in window.

use gpui::{
    App, AppContext as _, Bounds, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, TitlebarOptions, Window, WindowBounds,
    WindowHandle, WindowOptions, div, img, px, rgb, size,
};
use gpui_component::Disableable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};

use crate::window::{KitRoot, init};
use crate::{app_icon, theme};

/// The window's size: a small dialog of its own.
const SIZE: (f32, f32) = (480.0, 400.0);

/// The app's icon over the title, as Teams' own sign-in shows its logo.
const LOGO: f32 = 48.0;

/// What the field asks for when it is empty.
pub const PLACEHOLDER: &str = "name@company.com";

/// What the screen says when the field doesn't hold an account.
pub const NOT_AN_ACCOUNT: &str = "Enter your work account, like name@company.com";

/// What the screen says while Microsoft's sign-in window is open.
pub const SIGNING_IN: &str = "Finish signing in in Microsoft's window that opened.";

/// What the screen asks the app for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccountPromptEvent {
    /// Sign this account (the field's text, trimmed) in.
    SignIn(String),
}

/// The first sign-in screen (see the module docs).
pub struct AccountPrompt {
    input: Entity<InputState>,
    signing_in: bool,
    /// What the line under the field says, and the field's text when it was said: an edit clears it (the kit's input
    /// also tells a change after an Enter that changed nothing, which must not).
    problem: Option<(SharedString, String)>,
    _input_events: Subscription,
    /// The theme following the window's appearance ([`open`] makes it so).
    appearance: Option<Subscription>,
}

impl EventEmitter<AccountPromptEvent> for AccountPrompt {}

impl AccountPrompt {
    /// The screen, its field holding `account` (the one an earlier attempt named) and focused.
    pub fn new(account: Option<&str>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let account = account.unwrap_or_default().to_owned();
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(PLACEHOLDER)
                .default_value(account)
        });
        let input_events = cx.subscribe_in(
            &input,
            window,
            |this: &mut Self, _, event: &InputEvent, _, cx| match event {
                InputEvent::PressEnter { .. } => this.submit(cx),
                InputEvent::Change => this.clear_problem_once_edited(cx),
                _ => {}
            },
        );
        input.update(cx, |input, cx| input.focus(window, cx));
        Self {
            input,
            signing_in: false,
            problem: None,
            _input_events: input_events,
            appearance: None,
        }
    }

    /// The screen with the theme following its window's appearance (`appearance`, from [`theme::follow`]).
    #[must_use]
    pub fn following(mut self, appearance: Subscription) -> Self {
        self.appearance = Some(appearance);
        self
    }

    /// Whether the theme follows this screen's window.
    #[must_use]
    pub fn follows_appearance(&self) -> bool {
        self.appearance.is_some()
    }

    /// The account the field holds, trimmed.
    #[must_use]
    pub fn account(&self, cx: &App) -> String {
        self.input.read(cx).value().trim().to_owned()
    }

    /// Whether Microsoft's sign-in window is open for the account asked.
    #[must_use]
    pub fn is_signing_in(&self) -> bool {
        self.signing_in
    }

    /// What the line under the field says, if anything.
    #[must_use]
    pub fn problem(&self) -> Option<&SharedString> {
        self.problem.as_ref().map(|(problem, _)| problem)
    }

    /// The sign-in stored nothing (`reason`, said under the field): the field is open again.
    pub fn failed(&mut self, reason: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.signing_in = false;
        self.say(reason.into(), cx);
    }

    /// Says `problem` under the field, for the text the field holds now.
    fn say(&mut self, problem: SharedString, cx: &mut Context<Self>) {
        self.problem = Some((problem, self.account(cx)));
        cx.notify();
    }

    /// Clears the line under the field once the field's text differs from what it was said for.
    fn clear_problem_once_edited(&mut self, cx: &mut Context<Self>) {
        let account = self.account(cx);
        if self.problem.as_ref().is_some_and(|(_, said_for)| *said_for != account) {
            self.problem = None;
            cx.notify();
        }
    }

    /// Asks the app to sign the field's account in, unless a sign-in is already open or the field holds no account.
    pub fn submit(&mut self, cx: &mut Context<Self>) {
        if self.signing_in {
            return;
        }
        let account = self.account(cx);
        if !looks_like_account(&account) {
            self.say(NOT_AN_ACCOUNT.into(), cx);
            return;
        }
        self.signing_in = true;
        self.problem = None;
        cx.emit(AccountPromptEvent::SignIn(account));
        cx.notify();
    }
}

/// Whether `text` reads as an account (`name@domain`, no spaces): the app checks it fully before the sign-in.
fn looks_like_account(text: &str) -> bool {
    match text.split_once('@') {
        Some((name, domain)) => {
            !name.is_empty() && domain.contains('.') && !domain.contains('@') && !text.contains(char::is_whitespace)
        }
        None => false,
    }
}

impl Render for AccountPrompt {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let status = if self.signing_in {
            Some(SharedString::from(SIGNING_IN))
        } else {
            self.problem.as_ref().map(|(problem, _)| problem.clone())
        };
        let card = div()
            .size_full()
            .p_8()
            .flex()
            .flex_col()
            .gap_4()
            .rounded(px(theme::SHEET_RADIUS))
            .bg(rgb(palette.background))
            .shadow(theme::glow(palette, 0x0000_0000, 0.35, 32.0, 10.0))
            .child(
                img(app_icon::image())
                    .debug_selector(|| "account-prompt-logo".to_owned())
                    .size(px(LOGO))
                    .flex_none(),
            )
            .child(
                div()
                    .text_xl()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child("Sign in to Teams"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(palette.muted_foreground))
                    .child("Enter your work account. Microsoft's sign-in opens in a window of its own."),
            )
            .child(
                div()
                    .debug_selector(|| "account-prompt-field".to_owned())
                    .child(Input::new(&self.input).disabled(self.signing_in)),
            )
            .children(status.map(|status| {
                div()
                    .debug_selector(|| "account-prompt-status".to_owned())
                    .text_sm()
                    .child(status)
            }))
            .child(
                div().debug_selector(|| "account-prompt-sign-in".to_owned()).child(
                    Button::new("account-prompt-sign-in")
                        .primary()
                        .label("Sign in")
                        .disabled(self.signing_in)
                        .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                ),
            );
        // on the frame, the card rising in as the window opens
        div()
            .debug_selector(|| "account-prompt".to_owned())
            .size_full()
            .p(px(theme::SHEET_GAP * 2.0))
            .bg(theme::frame(palette))
            .text_color(rgb(palette.foreground))
            .child(crate::motion::rise(card, "account-prompt-card", 12.0, cx))
    }
}

/// Opens the first sign-in screen in a window of its own, titled `title`, with app id `app_id`, its field holding
/// `account`; sets the app up first ([`init`]) and keeps the theme following the window's appearance.
///
/// # Errors
/// The platform can't open a window.
pub fn open(
    cx: &mut App,
    app_id: &str,
    title: SharedString,
    account: Option<&str>,
) -> gpui::Result<(WindowHandle<KitRoot>, Entity<AccountPrompt>)> {
    let bounds = Bounds::centered(None, size(px(SIZE.0), px(SIZE.1)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some(title),
            ..TitlebarOptions::default()
        }),
        app_id: Some(app_id.to_owned()),
        ..WindowOptions::default()
    };
    init(cx);
    let mut prompt = None;
    let window = cx.open_window(options, |window, cx| {
        let following = theme::follow(window, cx);
        let view = cx.new(|cx| AccountPrompt::new(account, window, cx).following(following));
        prompt = Some(view.clone());
        cx.new(|cx| KitRoot::new(view, window, cx))
    })?;
    let prompt = prompt.ok_or_else(|| std::io::Error::other("the sign-in screen wasn't made"))?;
    Ok((window, prompt))
}

#[cfg(test)]
mod tests {
    use super::looks_like_account;

    #[test]
    fn an_account_is_a_name_at_a_domain() {
        assert!(looks_like_account("someone@example.test"));
        for text in [
            "",
            "someone",
            "@example.test",
            "someone@example",
            "some one@example.test",
            "a@b@c.test",
        ] {
            assert!(!looks_like_account(text), "{text:?}");
        }
    }
}
