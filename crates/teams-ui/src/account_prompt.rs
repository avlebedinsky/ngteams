//! The personal build's first sign-in screen (`docs/personal-build.md`, "How the implementation reads it"): the window
//! `ngteams` opens when no account is signed in yet, asking for the work account as Teams' own sign-in does, with "Sign
//! in". Enter or the button asks the app to sign that account in ([`AccountPromptEvent::SignIn`]); while Microsoft's
//! sign-in window is open the field and the button wait, and a sign-in that stored nothing comes back as a line under
//! the field ([`AccountPrompt::failed`]), the field open again. It sends nothing itself: the account goes only to the
//! app, which checks it and hands it to the sign-in window.
//!
//! **An own server** ([`AccountPrompt::offer_own_server`]): the screen then asks first where the account is,
//! "Microsoft 365" (the default, the flow above) or "Own server", which shows the server's address, the user name and
//! the password and asks the app to sign in there ([`AccountPromptEvent::SignInOwn`]); no Microsoft window opens for
//! it. The password leaves the screen only in that event and is cleared from the field when a sign-in fails. An own
//! server's account signed out is asked for its password alone ([`AccountPrompt::ask_own_again`]).
//!
//! **The keyboard**: Tab walks the choice, the fields and "Sign in" in the order they show. The choice is a radio
//! group ([`crate::keyboard`]): Tab stops on the place chosen, the arrows choose the other one with the keyboard
//! staying on it, and Enter or Space goes on to the chosen place's first field.

use gpui::{
    App, AppContext as _, Bounds, Context, Entity, EventEmitter, FocusHandle, InteractiveElement as _, IntoElement,
    KeyDownEvent, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription,
    TitlebarOptions, Window, WindowBounds, WindowHandle, WindowOptions, div, img, prelude::FluentBuilder as _, px, rgb,
    size,
};
use gpui_component::Disableable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};

use zeroize::Zeroizing;

use crate::keyboard::{self, Handles};
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

/// The window's height with the choice of where the account is and an own server's three fields.
const OWN_HEIGHT: f32 = 600.0;

/// What the server's field asks for when it is empty.
pub const SERVER_PLACEHOLDER: &str = "https://chat.example.com";

/// What the screen says when the server's field holds no address.
pub const NOT_A_SERVER: &str = "Enter your server's address, like https://chat.example.com";

/// What the screen says when the user name's field doesn't hold one.
pub const NOT_A_USER: &str = "Enter your user name on the server, like name@example.com";

/// What the screen says when the password's field is empty.
pub const NO_PASSWORD: &str = "Enter your password";

/// What the screen says while an own server's sign-in is under way.
pub const SIGNING_IN_OWN: &str = "Signing in…";

/// Where the account is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignInKind {
    Microsoft,
    OwnServer,
}

/// The places, in the order the choice shows them.
const KINDS: [SignInKind; 2] = [SignInKind::Microsoft, SignInKind::OwnServer];

/// Tab's order in the window: the choice, the fields (an own server's three count on from [`TAB_FIELDS`]), "Sign in".
const TAB_CHOICE: isize = 0;
const TAB_FIELDS: isize = 1;
const TAB_SIGN_IN: isize = 4;

impl SignInKind {
    /// Its key, in its segment's selector.
    fn key(self) -> &'static str {
        match self {
            Self::Microsoft => "microsoft",
            Self::OwnServer => "own",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Microsoft => "Microsoft 365",
            Self::OwnServer => "Own server",
        }
    }
}

/// An own server's sign-in as the screen's fields hold it.
#[derive(Clone)]
pub struct OwnSignIn {
    /// The server's address as typed, trimmed.
    pub server: String,
    /// The user name, trimmed.
    pub user: String,
    /// The password as typed, zeroed when dropped; never logged or shown.
    pub password: Zeroizing<String>,
}

impl std::fmt::Debug for OwnSignIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OwnSignIn")
            .field("server", &self.server)
            .field("user", &self.user)
            .finish_non_exhaustive()
    }
}

impl PartialEq for OwnSignIn {
    fn eq(&self, other: &Self) -> bool {
        self.server == other.server && self.user == other.user && *self.password == *other.password
    }
}

impl Eq for OwnSignIn {}

/// What the screen asks the app for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccountPromptEvent {
    /// Sign this account (the field's text, trimmed) in.
    SignIn(String),
    /// Sign this own server's account in with its password.
    SignInOwn(OwnSignIn),
}

/// How the screen stands to own servers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OwnAsk {
    No,
    Choice,
    Again,
}

/// Whether, and how, the screen asks for an own server's account.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum OwnOffer {
    /// Microsoft 365 alone, as before own servers.
    #[default]
    No,
    /// The choice between Microsoft 365 and an own server.
    Choice,
    /// This own server's account again: its password alone is asked.
    Again { server: String, user: String },
}

/// The first sign-in screen (see the module docs).
pub struct AccountPrompt {
    input: Entity<InputState>,
    server: Entity<InputState>,
    user: Entity<InputState>,
    password: Entity<InputState>,
    kind: SignInKind,
    /// The choice's segments' focus, which the arrows move.
    kinds: Handles,
    /// Whether the choice of where the account is shows ([`Self::offer_own_server`]), or an own server's account is
    /// asked again, its server and user name fixed ([`Self::ask_own_again`]).
    own: OwnAsk,
    signing_in: bool,
    /// What the line under the field says, and the field's text when it was said: an edit clears it (the kit's input
    /// also tells a change after an Enter that changed nothing, which must not).
    problem: Option<(SharedString, String)>,
    /// A failed sign-in's password is cleared from its field at the next draw (the field needs the window).
    clear_password: bool,
    _input_events: [Subscription; 4],
    /// The theme following the window's appearance ([`open_offering`] makes it so).
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
        let server = cx.new(|cx| InputState::new(window, cx).placeholder(SERVER_PLACEHOLDER));
        let user = cx.new(|cx| InputState::new(window, cx).placeholder(PLACEHOLDER));
        let password = cx.new(|cx| InputState::new(window, cx).masked(true));
        let input_events = [&input, &server, &user, &password].map(|field| {
            cx.subscribe_in(
                field,
                window,
                |this: &mut Self, _, event: &InputEvent, _, cx| match event {
                    InputEvent::PressEnter { .. } => this.submit(cx),
                    InputEvent::Change => this.clear_problem_once_edited(cx),
                    _ => {}
                },
            )
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        Self {
            input,
            server,
            user,
            password,
            kind: SignInKind::Microsoft,
            kinds: Handles::default(),
            own: OwnAsk::No,
            signing_in: false,
            problem: None,
            clear_password: false,
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

    /// Shows the choice of where the account is: Microsoft 365 (chosen) or an own server.
    pub fn offer_own_server(&mut self, cx: &mut Context<Self>) {
        self.own = OwnAsk::Choice;
        cx.notify();
    }

    /// Asks for the password of the own server `server`'s account `user` again: both shown, fixed, the password's
    /// field focused, and no choice of another place.
    pub fn ask_own_again(&mut self, server: &str, user: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.kind = SignInKind::OwnServer;
        self.own = OwnAsk::Again;
        let (server, user) = (server.to_owned(), user.to_owned());
        self.server.update(cx, |field, cx| field.set_value(server, window, cx));
        self.user.update(cx, |field, cx| field.set_value(user, window, cx));
        self.password.update(cx, |field, cx| field.focus(window, cx));
        cx.notify();
    }

    /// Where the account is, as chosen.
    #[must_use]
    pub fn kind(&self) -> SignInKind {
        self.kind
    }

    /// Chooses where the account is and focuses its first field; nothing while a sign-in is open, or for an account
    /// asked again.
    pub fn choose(&mut self, kind: SignInKind, window: &mut Window, cx: &mut Context<Self>) {
        if self.signing_in || self.own != OwnAsk::Choice {
            return;
        }
        self.set_kind(kind, cx);
        let first = match kind {
            SignInKind::Microsoft => &self.input,
            SignInKind::OwnServer => &self.server,
        };
        first.update(cx, |field, cx| field.focus(window, cx));
    }

    fn set_kind(&mut self, kind: SignInKind, cx: &mut Context<Self>) {
        if self.kind != kind {
            self.kind = kind;
            self.problem = None;
            cx.notify();
        }
    }

    /// The handle of `kind`'s segment in the choice.
    fn kind_focus(&self, kind: SignInKind, cx: &App) -> FocusHandle {
        self.kinds.get(kind.key(), cx).tab_index(TAB_CHOICE)
    }

    /// The choice's keys, as a radio group's: the arrows choose the other place, the keyboard on its segment; Enter
    /// or Space goes on to the place's first field.
    fn choice_key(&mut self, kind: SignInKind, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        if matches!(key, "enter" | "space") && !event.keystroke.modifiers.modified() {
            cx.stop_propagation();
            self.choose(kind, window, cx);
            return;
        }
        let at = KINDS.iter().position(|shown| *shown == kind).unwrap_or(0);
        let Some(to) = keyboard::step(event, KINDS.len(), at) else {
            return;
        };
        cx.stop_propagation();
        if self.signing_in {
            return;
        }
        let kind = KINDS[to];
        self.set_kind(kind, cx);
        self.kind_focus(kind, cx).focus(window, cx);
    }

    /// What the own server's fields hold: the server's address and the user name, trimmed.
    #[must_use]
    pub fn own_fields(&self, cx: &App) -> (String, String) {
        (
            self.server.read(cx).value().trim().to_owned(),
            self.user.read(cx).value().trim().to_owned(),
        )
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
        self.clear_password = true;
        self.say(reason.into(), cx);
    }

    /// Says `problem` under the field, for the text the field holds now.
    fn say(&mut self, problem: SharedString, cx: &mut Context<Self>) {
        self.problem = Some((problem, self.typed(cx)));
        cx.notify();
    }

    /// Clears the line under the field once the field's text differs from what it was said for.
    fn clear_problem_once_edited(&mut self, cx: &mut Context<Self>) {
        let typed = self.typed(cx);
        if self.problem.as_ref().is_some_and(|(_, said_for)| *said_for != typed) {
            self.problem = None;
            cx.notify();
        }
    }

    /// What the fields of the place chosen hold, to tell an edit by (the password by its length alone).
    fn typed(&self, cx: &App) -> String {
        match self.kind {
            SignInKind::Microsoft => self.account(cx),
            SignInKind::OwnServer => {
                let (server, user) = self.own_fields(cx);
                let password = self.password.read(cx).unmask_value().len();
                format!("{server}\n{user}\n{password}")
            }
        }
    }

    /// Asks the app to sign the own server's account in, unless a field doesn't hold what it asks for.
    fn submit_own(&mut self, cx: &mut Context<Self>) {
        let (server, user) = self.own_fields(cx);
        let password = Zeroizing::new(self.password.read(cx).unmask_value().to_string());
        let problem = if !looks_like_server(&server) {
            Some(NOT_A_SERVER)
        } else if !looks_like_account(&user) {
            Some(NOT_A_USER)
        } else if password.is_empty() {
            Some(NO_PASSWORD)
        } else {
            None
        };
        if let Some(problem) = problem {
            self.say(problem.into(), cx);
            return;
        }
        self.signing_in = true;
        self.problem = None;
        cx.emit(AccountPromptEvent::SignInOwn(OwnSignIn { server, user, password }));
        cx.notify();
    }

    /// Asks the app to sign the field's account in, unless a sign-in is already open or the field holds no account.
    pub fn submit(&mut self, cx: &mut Context<Self>) {
        if self.signing_in {
            return;
        }
        if self.kind == SignInKind::OwnServer {
            self.submit_own(cx);
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

/// Whether `text` reads as a server's address: a host, with `https://` before it or no scheme at all (the app puts
/// it there), and no spaces. The app checks it fully before the sign-in.
fn looks_like_server(text: &str) -> bool {
    let host = text.strip_prefix("https://").unwrap_or(text);
    !host.is_empty() && !host.contains("://") && !host.starts_with('/') && !text.contains(char::is_whitespace)
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

impl AccountPrompt {
    /// The choice of where the account is: two segments, the chosen one filled.
    fn choice(&self, palette: theme::Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let segments = KINDS.map(|kind| {
            let chosen = self.kind == kind;
            // the ring of the keyboard's place: on the chosen segment's fill, so in the fill's text colour
            let ring = if chosen { palette.on_brand } else { palette.brand };
            div()
                .debug_selector(move || format!("account-prompt-kind-{}", kind.key()))
                .id(kind.key())
                .track_focus(&self.kind_focus(kind, cx).tab_stop(chosen))
                .h(px(28.0))
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .rounded_md()
                .border_2()
                .border_color(rgb(ring).opacity(0.0))
                .focus_visible(move |segment| segment.border_color(rgb(ring)))
                .text_sm()
                .cursor_pointer()
                .when(chosen, |segment| {
                    segment
                        .bg(rgb(palette.brand))
                        .text_color(rgb(palette.on_brand))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                })
                .when(!chosen, |segment| {
                    segment.hover(move |segment| segment.bg(rgb(palette.hover)))
                })
                .child(kind.label())
                .on_click(cx.listener(move |this, _, window, cx| this.choose(kind, window, cx)))
                .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                    this.choice_key(kind, event, window, cx);
                }))
        });
        div()
            .debug_selector(|| "account-prompt-kind".to_owned())
            .flex()
            .items_center()
            .gap_0p5()
            .p_0p5()
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette.border))
            .children(segments)
    }

    /// An own server's three fields, each under its name.
    fn own_form(&self, palette: theme::Palette) -> impl IntoElement {
        let field = |key: &'static str, name: &'static str, input: Input| {
            div()
                .debug_selector(move || format!("account-prompt-{key}"))
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_xs().text_color(rgb(palette.muted_foreground)).child(name))
                .child(input)
        };
        let fixed = self.signing_in || self.own == OwnAsk::Again;
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(field(
                "server",
                "Server address",
                Input::new(&self.server).disabled(fixed).tab_index(TAB_FIELDS),
            ))
            .child(field(
                "user",
                "User name",
                Input::new(&self.user).disabled(fixed).tab_index(TAB_FIELDS + 1),
            ))
            .child(field(
                "password",
                "Password",
                Input::new(&self.password)
                    .mask_toggle()
                    .disabled(self.signing_in)
                    .tab_index(TAB_FIELDS + 2),
            ))
    }

    /// The words under the title, and what the status line says while a sign-in is under way.
    fn words(&self) -> (&'static str, &'static str) {
        match self.kind {
            SignInKind::Microsoft => (
                "Enter your work account. Microsoft's sign-in opens in a window of its own.",
                SIGNING_IN,
            ),
            SignInKind::OwnServer if self.own == OwnAsk::Again => {
                ("Enter your password to sign in again.", SIGNING_IN_OWN)
            }
            SignInKind::OwnServer => ("Enter your server's address and your account on it.", SIGNING_IN_OWN),
        }
    }
}

impl Render for AccountPrompt {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        if std::mem::take(&mut self.clear_password) {
            self.password.update(cx, |field, cx| field.set_value("", window, cx));
        }
        let (words, signing_in) = self.words();
        let status = if self.signing_in {
            Some(SharedString::from(signing_in))
        } else {
            self.problem.as_ref().map(|(problem, _)| problem.clone())
        };
        let fields = match self.kind {
            SignInKind::Microsoft => div()
                .debug_selector(|| "account-prompt-field".to_owned())
                .child(Input::new(&self.input).disabled(self.signing_in).tab_index(TAB_FIELDS))
                .into_any_element(),
            SignInKind::OwnServer => self.own_form(palette).into_any_element(),
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
            .when(self.own == OwnAsk::Choice, |card| card.child(self.choice(palette, cx)))
            .child(div().text_sm().text_color(rgb(palette.muted_foreground)).child(words))
            .child(fields)
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
                        .tab_index(TAB_SIGN_IN)
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
/// `account`, asking for an own server's account as `own` says (the window taller for its fields); sets the app up
/// first ([`init`]) and keeps the theme following the window's appearance.
///
/// # Errors
/// The platform can't open a window.
pub fn open_offering(
    cx: &mut App,
    app_id: &str,
    title: SharedString,
    account: Option<&str>,
    own: &OwnOffer,
) -> gpui::Result<(WindowHandle<KitRoot>, Entity<AccountPrompt>)> {
    let height = if *own == OwnOffer::No { SIZE.1 } else { OWN_HEIGHT };
    let bounds = Bounds::centered(None, size(px(SIZE.0), px(height)), cx);
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
        let view = cx.new(|cx| {
            let mut prompt = AccountPrompt::new(account, window, cx).following(following);
            match own {
                OwnOffer::No => {}
                OwnOffer::Choice => prompt.offer_own_server(cx),
                OwnOffer::Again { server, user } => prompt.ask_own_again(server, user, window, cx),
            }
            prompt
        });
        prompt = Some(view.clone());
        cx.new(|cx| KitRoot::new(view, window, cx))
    })?;
    let prompt = prompt.ok_or_else(|| std::io::Error::other("the sign-in screen wasn't made"))?;
    Ok((window, prompt))
}

#[cfg(test)]
mod tests {
    use super::{looks_like_account, looks_like_server};

    #[test]
    fn a_server_is_a_host_with_https_or_no_scheme() {
        for text in ["chat.example.test", "https://chat.example.test:8443", "localhost:8443"] {
            assert!(looks_like_server(text), "{text:?}");
        }
        for text in ["", "http://chat.example.test", "https://", "chat example.test", "/path"] {
            assert!(!looks_like_server(text), "{text:?}");
        }
    }

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
