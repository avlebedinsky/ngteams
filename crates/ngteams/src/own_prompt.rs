//! What the sign-in screen asks about an own server (`teams_ui::account_prompt`), done the same wherever the screen
//! is open: the first run, the rail's "+" and an account signed out.
//!
//! - **The server's address entered**: the server is asked whether it still needs its first administrator
//!   ([`crate::own_admin::needs_admin`]); if so the screen shows the form that creates one. A server that can't be
//!   asked says nothing here: the sign-in says why.
//! - **A sign-in**: the same question first for an account being added (a person who filled in the sign-in form on a
//!   new server gets the form, not "wrong password"), then the sign-in with the password, on that server alone. A
//!   password the server takes only to have it replaced ([`crate::signin::PASSWORD_MUST_CHANGE`]) turns the screen to
//!   its new-password form.
//! - **The first administrator**: created ([`crate::own_admin::create_first_admin`]), then signed in as any account.
//! - **A new password**: replaced on the server ([`crate::own_admin::change_password`]), then the sign-in with it.
//!
//! Everything runs off GPUI's foreground. A sign-in that went through is handed to [`OwnPrompt::signed`]; whatever
//! didn't is said on the screen. Nothing here asks a Microsoft host: a Microsoft 365 sign-in isn't this module's.

use std::rc::Rc;

use gpui::{App, AppContext as _, AsyncApp, Entity};
use teams_auth::secrets::Secret;
use teams_ui::account_prompt::{AccountPrompt, AccountPromptEvent, NewAdmin, OwnSignIn};

use crate::accounts;
use crate::accounts_file::{OwnServer, PersonalAccount};
use crate::own_admin;
use crate::setup::Setup;
use crate::signin::PASSWORD_MUST_CHANGE;

/// What follows a sign-in that went through: the account, and the screen it was typed on (for what still keeps the
/// account from showing).
pub type Signed = Rc<dyn Fn(PersonalAccount, Entity<AccountPrompt>, &mut App)>;

/// What signs an own server's account in and asks a server about its administrator: the app's own
/// ([`SignIns::app`]), or a test's stand-ins.
#[derive(Clone, Copy, Debug)]
pub struct SignIns {
    /// [`accounts::add_own`].
    pub add_own: fn(&Setup, &str, &OwnServer, &Secret) -> Result<PersonalAccount, String>,
    /// [`accounts::sign_in_own`].
    pub sign_in_own: fn(&Setup, &PersonalAccount, &Secret) -> Result<PersonalAccount, String>,
    /// [`own_admin::needs_admin`].
    pub needs_admin: fn(&OwnServer) -> Result<Option<bool>, String>,
}

impl SignIns {
    /// The app's own.
    #[must_use]
    pub fn app() -> Self {
        Self {
            add_own: accounts::add_own,
            sign_in_own: accounts::sign_in_own,
            needs_admin: own_admin::needs_admin,
        }
    }
}

/// The own-server side of one sign-in screen.
#[derive(Clone)]
pub struct OwnPrompt {
    pub setup: Setup,
    pub sign_ins: SignIns,
    /// The account the screen asks again (signed out, or never signed in); `None` where an account is added.
    pub again: Option<PersonalAccount>,
    pub signed: Signed,
}

/// How a sign-in from the screen ended.
enum Ended {
    Signed(PersonalAccount),
    /// The server has no administrator yet; whether creating one takes the setup code.
    NeedsAdmin(bool),
    MustChangePassword,
    Failed(String),
}

impl OwnPrompt {
    /// Does what `event` asks when it is about an own server; `false` for a Microsoft 365 sign-in, which the caller
    /// keeps.
    pub fn handle(&self, event: &AccountPromptEvent, prompt: Entity<AccountPrompt>, cx: &mut App) -> bool {
        match event {
            AccountPromptEvent::SignIn(_) => return false,
            AccountPromptEvent::ServerEntered { server, ca_file } => self.ask_server(server, ca_file, prompt, cx),
            AccountPromptEvent::SignInOwn(own) => self.sign_in(own.clone(), prompt, cx),
            AccountPromptEvent::CreateAdmin(new) => self.create_admin(new.clone(), prompt, cx),
            AccountPromptEvent::ChangePassword { own, new } => {
                self.change_password(own.clone(), Secret::new(new.as_str()), prompt, cx);
            }
        }
        true
    }

    /// The server the screen names: the account's own when one is asked again, else as typed.
    fn server(&self, typed: &str, ca_file: &str) -> Result<OwnServer, String> {
        match self.again.as_ref().and_then(PersonalAccount::own_server) {
            Some(server) => Ok(server),
            None => OwnServer::parse(typed, ca_file).map_err(|error| error.to_string()),
        }
    }

    fn ask_server(&self, typed: &str, ca_file: &str, prompt: Entity<AccountPrompt>, cx: &mut App) {
        let (Ok(server), true) = (OwnServer::parse(typed, ca_file), self.setup.own_servers()) else {
            return;
        };
        let typed = typed.to_owned();
        let needs_admin = self.sign_ins.needs_admin;
        let asking = cx.background_spawn(async move { needs_admin(&server) });
        cx.spawn(async move |cx: &mut AsyncApp| {
            if let Ok(Some(code_required)) = asking.await {
                cx.update(|cx| prompt.update(cx, |prompt, cx| prompt.needs_admin(&typed, code_required, cx)));
            }
        })
        .detach();
    }

    /// Shows how a sign-in ended on `prompt` (`typed`: the server's address as the screen holds it).
    fn end(&self, ended: Ended, typed: &str, prompt: Entity<AccountPrompt>, cx: &mut App) {
        match ended {
            Ended::Signed(account) => (self.signed)(account, prompt, cx),
            Ended::NeedsAdmin(code_required) => {
                prompt.update(cx, |prompt, cx| prompt.needs_admin(typed, code_required, cx));
            }
            Ended::MustChangePassword => prompt.update(cx, AccountPrompt::must_change_password),
            Ended::Failed(error) => {
                eprintln!("teams: {error}");
                prompt.update(cx, |prompt, cx| prompt.failed(error, cx));
            }
        }
    }

    fn sign_in(&self, own: OwnSignIn, prompt: Entity<AccountPrompt>, cx: &mut App) {
        let server = match self.server(&own.server, &own.ca_file) {
            Ok(server) => server,
            Err(error) => {
                prompt.update(cx, |prompt, cx| prompt.failed(error, cx));
                return;
            }
        };
        let (this, typed) = (self.clone_for_work(), own.server.clone());
        let signing = cx.background_spawn(async move {
            // a new server is told apart from a wrong password before anything is typed against it
            if this.again.is_none()
                && let Ok(Some(code_required)) = (this.sign_ins.needs_admin)(&server)
            {
                return Ended::NeedsAdmin(code_required);
            }
            this.signed_in(&server, &own.user, &Secret::new(own.password.as_str()))
        });
        let this = self.clone();
        cx.spawn(async move |cx: &mut AsyncApp| {
            let ended = signing.await;
            cx.update(|cx| this.end(ended, &typed, prompt, cx));
        })
        .detach();
    }

    fn create_admin(&self, new: NewAdmin, prompt: Entity<AccountPrompt>, cx: &mut App) {
        let server = match OwnServer::parse(&new.server, &new.ca_file) {
            Ok(server) => server,
            Err(error) => {
                prompt.update(cx, |prompt, cx| {
                    prompt.setup_failed(error.to_string(), false, false, cx);
                });
                return;
            }
        };
        let this = self.clone_for_work();
        let creating = cx.background_spawn(async move {
            let password = Secret::new(new.password.as_str());
            let code = new.code.as_ref().map(|code| Secret::new(code.as_str()));
            let admin = own_admin::NewAdmin {
                code: code.as_ref(),
                upn: &new.user,
                display_name: &new.name,
                password: &password,
            };
            own_admin::create_first_admin(&server, admin)?;
            // the account exists from here: a sign-in that fails leaves the sign-in form, not this one
            match this.signed_in(&server, &new.user, &password) {
                Ended::Signed(account) => Ok(account),
                Ended::Failed(words) => Err(own_admin::SetupFailed {
                    words,
                    code: false,
                    admin_exists: true,
                }),
                Ended::NeedsAdmin(_) | Ended::MustChangePassword => Err(own_admin::SetupFailed {
                    words: own_admin::ADMIN_EXISTS.to_owned(),
                    code: false,
                    admin_exists: true,
                }),
            }
        });
        let signed = Rc::clone(&self.signed);
        cx.spawn(async move |cx: &mut AsyncApp| {
            let created = creating.await;
            cx.update(|cx| match created {
                Ok(account) => signed(account, prompt, cx),
                Err(failed) => prompt.update(cx, |prompt, cx| {
                    prompt.setup_failed(failed.words, failed.code, failed.admin_exists, cx);
                }),
            });
        })
        .detach();
    }

    fn change_password(&self, own: OwnSignIn, new: Secret, prompt: Entity<AccountPrompt>, cx: &mut App) {
        let server = match self.server(&own.server, &own.ca_file) {
            Ok(server) => server,
            Err(error) => {
                prompt.update(cx, |prompt, cx| prompt.failed(error, cx));
                return;
            }
        };
        let (this, typed) = (self.clone_for_work(), own.server.clone());
        let changing = cx.background_spawn(async move {
            let current = Secret::new(own.password.as_str());
            match own_admin::change_password(&server, &own.user, &current, &new) {
                Ok(()) => this.signed_in(&server, &own.user, &new),
                Err(error) => Ended::Failed(error),
            }
        });
        let this = self.clone();
        cx.spawn(async move |cx: &mut AsyncApp| {
            let ended = changing.await;
            cx.update(|cx| this.end(ended, &typed, prompt, cx));
        })
        .detach();
    }

    /// What the work off the foreground needs: the setup and the account asked again (the `signed` callback stays on
    /// the foreground).
    fn clone_for_work(&self) -> Work {
        Work {
            setup: self.setup.clone(),
            again: self.again.clone(),
            sign_ins: self.sign_ins,
        }
    }
}

/// The part of [`OwnPrompt`] that crosses to a background thread.
struct Work {
    setup: Setup,
    again: Option<PersonalAccount>,
    sign_ins: SignIns,
}

impl Work {
    /// Signs `user` in on `server` with `password`: the account asked again, else one added.
    fn signed_in(&self, server: &OwnServer, user: &str, password: &Secret) -> Ended {
        let signed = match &self.again {
            Some(account) => (self.sign_ins.sign_in_own)(&self.setup, account, password),
            None => (self.sign_ins.add_own)(&self.setup, user, server, password),
        };
        match signed {
            Ok(account) => Ended::Signed(account),
            Err(error) if error == PASSWORD_MUST_CHANGE => Ended::MustChangePassword,
            Err(error) => Ended::Failed(error),
        }
    }
}
