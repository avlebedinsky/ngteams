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
//! server's account signed out is asked for its password alone ([`AccountPrompt::ask_own_again`]). A deployment with
//! its own certificate authority names that file in the form's last field.
//!
//! **A server without an administrator** ([`AccountPrompt::needs_admin`]): once the server's address is entered the
//! screen asks the app about it ([`AccountPromptEvent::ServerEntered`]), and when the app says it has no administrator
//! yet the screen says so and shows the form that creates one ([`Stage::Setup`]: the setup code when the server asks
//! one, user name, display name, the password twice; [`AccountPromptEvent::CreateAdmin`]).
//!
//! **A password that must change** ([`AccountPrompt::must_change_password`]): when the server takes the password
//! only to have it replaced, the screen asks for the new one twice ([`Stage::NewPassword`]) and hands both to the app
//! ([`AccountPromptEvent::ChangePassword`]), which changes it and signs in.
//!
//! **The keyboard**: Tab walks the choice, the fields and the buttons in the order they show. The choice is a radio
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

/// The window's height with the choice of where the account is and an own server's fields.
const OWN_HEIGHT: f32 = 740.0;

/// The shortest password an own server takes.
const MIN_PASSWORD: usize = 8;

/// What the certificate authority's field asks for when it is empty.
pub const CA_PLACEHOLDER: &str = if cfg!(windows) {
    r"C:\path\to\ca.pem"
} else {
    "/path/to/ca.pem"
};

/// What the screen says of a server that has no administrator yet.
pub const NO_ADMIN_YET: &str =
    "This server has no administrator yet. Create the administrator account to finish setting it up.";

/// What the screen says to a user whose password must change.
pub const MUST_CHANGE: &str = "Your administrator asks you to choose a new password before you sign in.";

/// What the screen says when the setup code's field is empty.
pub const NO_SETUP_CODE: &str = "Enter the setup code the server printed when it started.";

/// What the screen says when the display name's field is empty.
pub const NO_DISPLAY_NAME: &str = "Enter the name others see.";

/// What the screen says of a password that is too short.
pub const SHORT_PASSWORD: &str = "Use a password of at least 8 characters.";

/// What the screen says when the password typed twice differs.
pub const PASSWORDS_DIFFER: &str = "The passwords don't match.";

/// What the screen says when the new password is the one that must change.
pub const SAME_PASSWORD: &str = "Choose a password that isn't the current one.";

/// What the screen says while the first administrator is being created, and while a password is being changed.
pub const CREATING_ADMIN: &str = "Creating the account…";
pub const CHANGING_PASSWORD: &str = "Changing your password…";

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
/// Asked of an own server's account that signed in before.
pub const OWN_PASSWORD_AGAIN: &str = "Enter your password to sign in again.";
/// Asked of an own server's account that never signed in.
pub const OWN_PASSWORD_FIRST: &str = "Enter your password to sign in.";

/// Where the account is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignInKind {
    Microsoft,
    OwnServer,
}

/// The places, in the order the choice shows them.
const KINDS: [SignInKind; 2] = [SignInKind::Microsoft, SignInKind::OwnServer];

/// Tab's order in the window: the choice, the fields (a form's count on from [`TAB_FIELDS`]), then its buttons.
const TAB_CHOICE: isize = 0;
const TAB_FIELDS: isize = 1;
const TAB_SIGN_IN: isize = 6;
const TAB_BACK: isize = 7;

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
    /// The deployment's certificate authority file as typed, trimmed; empty for none.
    pub ca_file: String,
}

/// The first administrator of an own server, as the form holds it.
#[derive(Clone, PartialEq, Eq)]
pub struct NewAdmin {
    pub server: String,
    pub ca_file: String,
    /// The setup code, when the server asks one.
    pub code: Option<Zeroizing<String>>,
    pub user: String,
    pub name: String,
    pub password: Zeroizing<String>,
}

impl std::fmt::Debug for NewAdmin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewAdmin")
            .field("server", &self.server)
            .field("user", &self.user)
            .finish_non_exhaustive()
    }
}

/// Which form the screen shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// The sign-in.
    SignIn,
    /// The server's first administrator is created; with the setup code when the server asks one.
    Setup { code_required: bool },
    /// The password that must change is replaced.
    NewPassword,
}

impl Stage {
    /// Its name, for a test's log.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::SignIn => "sign-in",
            Self::Setup { .. } => "setup",
            Self::NewPassword => "new-password",
        }
    }
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
        self.server == other.server
            && self.user == other.user
            && *self.password == *other.password
            && self.ca_file == other.ca_file
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
    /// An own server's address was entered (with this certificate authority file, or none): the app says whether it
    /// still needs its first administrator ([`AccountPrompt::needs_admin`]).
    ServerEntered { server: String, ca_file: String },
    /// Create this own server's first administrator, then sign it in.
    CreateAdmin(NewAdmin),
    /// Replace the password of this own server's account (`own` holds the current one) with `new`, then sign it in.
    ChangePassword { own: OwnSignIn, new: Zeroizing<String> },
}

/// How the screen stands to own servers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OwnAsk {
    No,
    Choice,
    /// One own server's account, its server and user name fixed: `again` when it signed in before.
    Account {
        again: bool,
    },
}

/// Whether, and how, the screen asks for an own server's account.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum OwnOffer {
    /// Microsoft 365 alone, as before own servers.
    #[default]
    No,
    /// The choice between Microsoft 365 and an own server.
    Choice,
    /// This own server's account: its password alone is asked.
    Again { server: String, user: String },
    /// As [`Self::Again`], for an account that never signed in (an entry written by hand): no "again" is said.
    First { server: String, user: String },
}

/// What a draw owes the screen's fields.
#[derive(Clone, Copy, Debug, Default)]
struct Owed {
    /// A failed sign-in's password is cleared from its field.
    clear_password: bool,
    /// A wrong setup code is cleared from its field, and the field takes the keyboard.
    clear_code: bool,
    /// The form's first field takes the keyboard.
    focus_first: bool,
}

/// The first sign-in screen (see the module docs).
pub struct AccountPrompt {
    input: Entity<InputState>,
    server: Entity<InputState>,
    user: Entity<InputState>,
    password: Entity<InputState>,
    ca_file: Entity<InputState>,
    /// The setup code, the display name, and a new password typed twice: the forms of [`Stage::Setup`] and
    /// [`Stage::NewPassword`].
    code: Entity<InputState>,
    name: Entity<InputState>,
    new_password: Entity<InputState>,
    confirm: Entity<InputState>,
    stage: Stage,
    /// The server and certificate authority file the app was last asked about ([`AccountPromptEvent::ServerEntered`]).
    asked_about: Option<(String, String)>,
    /// The sign-in the server refused for a password that must change, kept until it is replaced.
    refused: Option<OwnSignIn>,
    /// What the next draw owes the fields (they need the window).
    owed: Owed,
    kind: SignInKind,
    /// The choice's segments' focus, which the arrows move.
    kinds: Handles,
    /// Whether the choice of where the account is shows ([`Self::offer_own_server`]), or an own server's account is
    /// asked alone, its server and user name fixed ([`Self::ask_own_again`], [`Self::ask_own_first`]).
    own: OwnAsk,
    signing_in: bool,
    /// What the line under the field says, and the field's text when it was said: an edit clears it (the kit's input
    /// also tells a change after an Enter that changed nothing, which must not).
    problem: Option<(SharedString, String)>,
    _input_events: [Subscription; 9],
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
        let ca_file = cx.new(|cx| InputState::new(window, cx).placeholder(CA_PLACEHOLDER));
        let code = cx.new(|cx| InputState::new(window, cx).placeholder("XXXXX-XXXXX-XXXXX-XXXXX-XXXXX"));
        let name = cx.new(|cx| InputState::new(window, cx));
        let new_password = cx.new(|cx| InputState::new(window, cx).masked(true));
        let confirm = cx.new(|cx| InputState::new(window, cx).masked(true));
        let fields = [
            &input,
            &server,
            &user,
            &password,
            &ca_file,
            &code,
            &name,
            &new_password,
            &confirm,
        ];
        let input_events = fields.map(|field| {
            cx.subscribe_in(
                field,
                window,
                |this: &mut Self, _, event: &InputEvent, _, cx| match event {
                    InputEvent::PressEnter { .. } => this.submit(cx),
                    InputEvent::Change => this.clear_problem_once_edited(cx),
                    InputEvent::Blur => this.server_entered(cx),
                    InputEvent::Focus => {}
                },
            )
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        Self {
            input,
            server,
            user,
            password,
            ca_file,
            code,
            name,
            new_password,
            confirm,
            stage: Stage::SignIn,
            asked_about: None,
            refused: None,
            owed: Owed::default(),
            kind: SignInKind::Microsoft,
            kinds: Handles::default(),
            own: OwnAsk::No,
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

    /// Shows the choice of where the account is: Microsoft 365 (chosen) or an own server.
    pub fn offer_own_server(&mut self, cx: &mut Context<Self>) {
        self.own = OwnAsk::Choice;
        cx.notify();
    }

    /// Asks for the password of the own server `server`'s account `user` again: both shown, fixed, the password's
    /// field focused, and no choice of another place.
    pub fn ask_own_again(&mut self, server: &str, user: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.ask_own(server, user, true, window, cx);
    }

    /// As [`Self::ask_own_again`] for an account that never signed in: the words don't say "again".
    pub fn ask_own_first(&mut self, server: &str, user: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.ask_own(server, user, false, window, cx);
    }

    fn ask_own(&mut self, server: &str, user: &str, again: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.kind = SignInKind::OwnServer;
        self.own = OwnAsk::Account { again };
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
        if self.signing_in || self.own != OwnAsk::Choice || self.stage != Stage::SignIn {
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

    /// The certificate authority file the own server's form names, trimmed; empty for none.
    #[must_use]
    pub fn ca_file(&self, cx: &App) -> String {
        self.ca_file.read(cx).value().trim().to_owned()
    }

    /// Which form shows.
    #[must_use]
    pub fn stage(&self) -> Stage {
        self.stage
    }

    /// The display name the first administrator's form holds, trimmed.
    #[must_use]
    pub fn display_name(&self, cx: &App) -> String {
        self.name.read(cx).value().trim().to_owned()
    }

    /// Asks the app about the own server the fields name, once per address and certificate authority file, while
    /// the sign-in form of the choice shows.
    fn server_entered(&mut self, cx: &mut Context<Self>) {
        if self.kind != SignInKind::OwnServer
            || self.own != OwnAsk::Choice
            || self.stage != Stage::SignIn
            || self.signing_in
        {
            return;
        }
        let (server, _) = self.own_fields(cx);
        let entered = (server, self.ca_file(cx));
        if !looks_like_server(&entered.0) || self.asked_about.as_ref() == Some(&entered) {
            return;
        }
        self.asked_about = Some(entered.clone());
        cx.emit(AccountPromptEvent::ServerEntered {
            server: entered.0,
            ca_file: entered.1,
        });
    }

    /// The own server `server` has no administrator yet (the app's answer to
    /// [`AccountPromptEvent::ServerEntered`], or to a sign-in there): the form that creates one shows, with the setup
    /// code's field when `code_required`. Nothing when the fields name another server by now.
    pub fn needs_admin(&mut self, server: &str, code_required: bool, cx: &mut Context<Self>) {
        let (held, _) = self.own_fields(cx);
        if self.kind != SignInKind::OwnServer || self.own != OwnAsk::Choice || held != server {
            return;
        }
        self.signing_in = false;
        self.problem = None;
        self.stage = Stage::Setup { code_required };
        self.owed.focus_first = true;
        cx.notify();
    }

    /// The sign-in was refused because the password must change: the form asking for a new one shows. Nothing
    /// unless a sign-in of an own server's account is under way.
    pub fn must_change_password(&mut self, cx: &mut Context<Self>) {
        if self.refused.is_none() {
            return;
        }
        self.signing_in = false;
        self.problem = None;
        self.stage = Stage::NewPassword;
        self.owed.focus_first = true;
        self.owed.clear_password = true;
        cx.notify();
    }

    /// The first administrator wasn't created (`reason`, said under the form): a wrong setup code (`code`) is cleared
    /// and asked again; a server that has an administrator by now (`admin_exists`) gets the sign-in form back.
    pub fn setup_failed(
        &mut self,
        reason: impl Into<SharedString>,
        code: bool,
        admin_exists: bool,
        cx: &mut Context<Self>,
    ) {
        self.signing_in = false;
        if admin_exists {
            self.stage = Stage::SignIn;
            self.owed.focus_first = true;
        }
        self.owed.clear_code = code;
        self.say(reason.into(), cx);
    }

    /// Back to the sign-in form, from the first administrator's or the new password's.
    pub fn back(&mut self, cx: &mut Context<Self>) {
        if self.signing_in || self.stage == Stage::SignIn {
            return;
        }
        self.stage = Stage::SignIn;
        self.refused = None;
        self.problem = None;
        // the server is asked again when its address is entered again
        self.asked_about = None;
        self.owed.focus_first = true;
        cx.notify();
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
        if self.stage == Stage::SignIn {
            self.owed.clear_password = true;
            self.refused = None;
        }
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

    /// What the fields of the form shown hold, to tell an edit by (a password by its length alone).
    fn typed(&self, cx: &App) -> String {
        let length = |field: &Entity<InputState>| field.read(cx).unmask_value().len();
        match (self.kind, self.stage) {
            (SignInKind::Microsoft, _) => self.account(cx),
            (SignInKind::OwnServer, Stage::SignIn) => {
                let (server, user) = self.own_fields(cx);
                format!("{server}\n{user}\n{}\n{}", length(&self.password), self.ca_file(cx))
            }
            (SignInKind::OwnServer, Stage::Setup { .. }) => {
                let (_, user) = self.own_fields(cx);
                format!(
                    "{user}\n{}\n{}\n{}\n{}",
                    self.display_name(cx),
                    length(&self.code),
                    length(&self.new_password),
                    length(&self.confirm)
                )
            }
            (SignInKind::OwnServer, Stage::NewPassword) => {
                format!("{}\n{}", length(&self.new_password), length(&self.confirm))
            }
        }
    }

    /// The new password typed twice, unless it is too short or the two differ.
    fn new_password(&self, cx: &App) -> Result<Zeroizing<String>, &'static str> {
        let new = Zeroizing::new(self.new_password.read(cx).unmask_value().to_string());
        let confirm = Zeroizing::new(self.confirm.read(cx).unmask_value().to_string());
        if new.chars().count() < MIN_PASSWORD {
            Err(SHORT_PASSWORD)
        } else if *new != *confirm {
            Err(PASSWORDS_DIFFER)
        } else {
            Ok(new)
        }
    }

    /// Asks the app to create the server's first administrator, unless a field doesn't hold what it asks for.
    fn submit_setup(&mut self, code_required: bool, cx: &mut Context<Self>) {
        let (server, user) = self.own_fields(cx);
        let name = self.display_name(cx);
        let code = Zeroizing::new(self.code.read(cx).value().trim().to_owned());
        let password = if code_required && code.is_empty() {
            Err(NO_SETUP_CODE)
        } else if !looks_like_account(&user) {
            Err(NOT_A_USER)
        } else if name.is_empty() {
            Err(NO_DISPLAY_NAME)
        } else {
            self.new_password(cx)
        };
        match password {
            Ok(password) => {
                self.signing_in = true;
                self.problem = None;
                cx.emit(AccountPromptEvent::CreateAdmin(NewAdmin {
                    server,
                    ca_file: self.ca_file(cx),
                    code: code_required.then_some(code),
                    user,
                    name,
                    password,
                }));
                cx.notify();
            }
            Err(problem) => self.say(problem.into(), cx),
        }
    }

    /// Asks the app to replace the password that must change, unless the new one doesn't do.
    fn submit_new_password(&mut self, cx: &mut Context<Self>) {
        let Some(own) = self.refused.clone() else {
            self.back(cx);
            return;
        };
        let new = self.new_password(cx).and_then(|new| {
            if *new == *own.password {
                Err(SAME_PASSWORD)
            } else {
                Ok(new)
            }
        });
        match new {
            Ok(new) => {
                self.signing_in = true;
                self.problem = None;
                cx.emit(AccountPromptEvent::ChangePassword { own, new });
                cx.notify();
            }
            Err(problem) => self.say(problem.into(), cx),
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
        let own = OwnSignIn {
            server,
            user,
            password,
            ca_file: self.ca_file(cx),
        };
        // kept in case the server takes the password only to have it replaced
        self.refused = Some(own.clone());
        cx.emit(AccountPromptEvent::SignInOwn(own));
        cx.notify();
    }

    /// Asks the app to sign the field's account in, unless a sign-in is already open or the field holds no account.
    pub fn submit(&mut self, cx: &mut Context<Self>) {
        if self.signing_in {
            return;
        }
        if self.kind == SignInKind::OwnServer {
            match self.stage {
                Stage::SignIn => self.submit_own(cx),
                Stage::Setup { code_required } => self.submit_setup(code_required, cx),
                Stage::NewPassword => self.submit_new_password(cx),
            }
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

    /// A field under its name.
    fn field(key: &'static str, name: &'static str, input: Input, palette: theme::Palette) -> gpui::Div {
        div()
            .debug_selector(move || format!("account-prompt-{key}"))
            .flex()
            .flex_col()
            .gap_1()
            .child(div().text_xs().text_color(rgb(palette.muted_foreground)).child(name))
            .child(input)
    }

    /// An own server's fields, each under its name: its address, the user name, the password and, while the place is
    /// chosen here, the deployment's certificate authority file.
    fn own_form(&self, palette: theme::Palette) -> gpui::Div {
        let fixed = self.signing_in || matches!(self.own, OwnAsk::Account { .. });
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(Self::field(
                "server",
                "Server address",
                Input::new(&self.server).disabled(fixed).tab_index(TAB_FIELDS),
                palette,
            ))
            .child(Self::field(
                "user",
                "User name",
                Input::new(&self.user).disabled(fixed).tab_index(TAB_FIELDS + 1),
                palette,
            ))
            .child(Self::field(
                "password",
                "Password",
                Input::new(&self.password)
                    .mask_toggle()
                    .disabled(self.signing_in)
                    .tab_index(TAB_FIELDS + 2),
                palette,
            ))
            .when(self.own == OwnAsk::Choice, |form| {
                form.child(Self::field(
                    "ca-file",
                    "Certificate authority file (optional)",
                    Input::new(&self.ca_file)
                        .disabled(self.signing_in)
                        .tab_index(TAB_FIELDS + 3),
                    palette,
                ))
            })
    }

    /// The server a form other than the sign-in's is about, on a line.
    fn server_line(&self, palette: theme::Palette, cx: &App) -> gpui::Div {
        let (server, _) = self.own_fields(cx);
        div()
            .debug_selector(|| "account-prompt-server-line".to_owned())
            .text_sm()
            .text_color(rgb(palette.muted_foreground))
            .child(format!("Server: {server}"))
    }

    /// A new password typed twice, the first field at Tab's place `at`.
    fn new_password_fields(&self, at: isize, palette: theme::Palette) -> [gpui::Div; 2] {
        [
            Self::field(
                "new-password",
                "Password",
                Input::new(&self.new_password)
                    .mask_toggle()
                    .disabled(self.signing_in)
                    .tab_index(at),
                palette,
            ),
            Self::field(
                "confirm",
                "Confirm password",
                Input::new(&self.confirm)
                    .mask_toggle()
                    .disabled(self.signing_in)
                    .tab_index(at + 1),
                palette,
            ),
        ]
    }

    /// The first administrator's form: the setup code when the server asks one, the user name, the display name and
    /// the password twice.
    fn setup_form(&self, code_required: bool, palette: theme::Palette, cx: &App) -> gpui::Div {
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(self.server_line(palette, cx))
            .when(code_required, |form| {
                form.child(Self::field(
                    "code",
                    "Setup code",
                    Input::new(&self.code).disabled(self.signing_in).tab_index(TAB_FIELDS),
                    palette,
                ))
            })
            .child(Self::field(
                "user",
                "User name",
                Input::new(&self.user)
                    .disabled(self.signing_in)
                    .tab_index(TAB_FIELDS + 1),
                palette,
            ))
            .child(Self::field(
                "name",
                "Display name",
                Input::new(&self.name)
                    .disabled(self.signing_in)
                    .tab_index(TAB_FIELDS + 2),
                palette,
            ))
            .children(self.new_password_fields(TAB_FIELDS + 3, palette))
    }

    /// The form of a password that must change: the new one twice.
    fn new_password_form(&self, palette: theme::Palette, cx: &App) -> gpui::Div {
        let (_, user) = self.own_fields(cx);
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(self.server_line(palette, cx))
            .child(
                div()
                    .debug_selector(|| "account-prompt-user-line".to_owned())
                    .text_sm()
                    .text_color(rgb(palette.muted_foreground))
                    .child(format!("User name: {user}")),
            )
            .children(self.new_password_fields(TAB_FIELDS, palette))
    }

    /// The title, the words under it, what the status line says while the app works, and the button's word.
    fn words(&self) -> [&'static str; 4] {
        match (self.kind, self.stage) {
            (SignInKind::Microsoft, _) => [
                "Sign in to Teams",
                "Enter your work account. Microsoft's sign-in opens in a window of its own.",
                SIGNING_IN,
                "Sign in",
            ],
            (SignInKind::OwnServer, Stage::SignIn) => {
                let asked = match self.own {
                    OwnAsk::Account { again: true } => OWN_PASSWORD_AGAIN,
                    OwnAsk::Account { again: false } => OWN_PASSWORD_FIRST,
                    OwnAsk::No | OwnAsk::Choice => "Enter your server's address and your account on it.",
                };
                ["Sign in to Teams", asked, SIGNING_IN_OWN, "Sign in"]
            }
            (SignInKind::OwnServer, Stage::Setup { .. }) => [
                "Create the administrator account",
                NO_ADMIN_YET,
                CREATING_ADMIN,
                "Create account",
            ],
            (SignInKind::OwnServer, Stage::NewPassword) => [
                "Choose a new password",
                MUST_CHANGE,
                CHANGING_PASSWORD,
                "Change password and sign in",
            ],
        }
    }

    /// What the screen asks for, as the line under its title says it.
    #[must_use]
    pub fn asks(&self) -> &'static str {
        self.words()[1]
    }

    /// What a draw owes the fields: a failed sign-in's password and a wrong setup code cleared, and the keyboard on
    /// the form's first field after the form changed.
    fn settle_fields(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.owed.clear_password) {
            self.password.update(cx, |field, cx| field.set_value("", window, cx));
        }
        if std::mem::take(&mut self.owed.clear_code) {
            self.code.update(cx, |field, cx| {
                field.set_value("", window, cx);
                field.focus(window, cx);
            });
        }
        if std::mem::take(&mut self.owed.focus_first) {
            let first = match (self.kind, self.stage) {
                (SignInKind::Microsoft, _) => &self.input,
                (SignInKind::OwnServer, Stage::SignIn) => match self.own {
                    OwnAsk::Account { .. } => &self.password,
                    OwnAsk::No | OwnAsk::Choice => &self.server,
                },
                (SignInKind::OwnServer, Stage::Setup { code_required: true }) => &self.code,
                (SignInKind::OwnServer, Stage::Setup { code_required: false }) => &self.user,
                (SignInKind::OwnServer, Stage::NewPassword) => &self.new_password,
            };
            first.update(cx, |field, cx| field.focus(window, cx));
        }
    }

    /// The form's buttons: the one that sends it and, off the sign-in form, "Back".
    fn buttons(&self, send: &'static str, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div().debug_selector(|| "account-prompt-sign-in".to_owned()).child(
                    Button::new("account-prompt-sign-in")
                        .primary()
                        .label(send)
                        .tab_index(TAB_SIGN_IN)
                        .disabled(self.signing_in)
                        .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                ),
            )
            .when(self.stage != Stage::SignIn, |row| {
                row.child(
                    div().debug_selector(|| "account-prompt-back".to_owned()).child(
                        Button::new("account-prompt-back")
                            .outline()
                            .label("Back")
                            .tab_index(TAB_BACK)
                            .disabled(self.signing_in)
                            .on_click(cx.listener(|this, _, _, cx| this.back(cx))),
                    ),
                )
            })
    }
}

impl Render for AccountPrompt {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        self.settle_fields(window, cx);
        let [title, words, working, send] = self.words();
        let status = if self.signing_in {
            Some(SharedString::from(working))
        } else {
            self.problem.as_ref().map(|(problem, _)| problem.clone())
        };
        let sign_in = self.stage == Stage::SignIn || self.kind == SignInKind::Microsoft;
        let fields = match (self.kind, self.stage) {
            (SignInKind::Microsoft, _) => div()
                .debug_selector(|| "account-prompt-field".to_owned())
                .child(Input::new(&self.input).disabled(self.signing_in).tab_index(TAB_FIELDS)),
            (SignInKind::OwnServer, Stage::SignIn) => self.own_form(palette),
            (SignInKind::OwnServer, Stage::Setup { code_required }) => self.setup_form(code_required, palette, cx),
            (SignInKind::OwnServer, Stage::NewPassword) => self.new_password_form(palette, cx),
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
            .when(sign_in, |card| {
                card.child(
                    img(app_icon::image())
                        .debug_selector(|| "account-prompt-logo".to_owned())
                        .size(px(LOGO))
                        .flex_none(),
                )
            })
            .child(
                div()
                    .debug_selector(|| "account-prompt-title".to_owned())
                    .text_xl()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(title),
            )
            .when(self.own == OwnAsk::Choice && sign_in, |card| {
                card.child(self.choice(palette, cx))
            })
            .child(div().text_sm().text_color(rgb(palette.muted_foreground)).child(words))
            .child(fields)
            .children(status.map(|status| {
                div()
                    .debug_selector(|| "account-prompt-status".to_owned())
                    .text_sm()
                    .child(status)
            }))
            .child(self.buttons(send, cx));
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
                OwnOffer::First { server, user } => prompt.ask_own_first(server, user, window, cx),
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
