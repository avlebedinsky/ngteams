//! The personal fence (`docs/personal-build.md`): its files, its keep-apart checks, and its decision, which is none: it
//! logs every write and permits it, reading no members, while the lab fence keeps its rule over the same targets.
//!
//! No real MRI or token appears here.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::Value;
use teams_fence::lab::LabLocations;
use teams_fence::personal::{PersonalAccount, PersonalError, PersonalPaths};
use teams_fence::{
    CallOperation, DenyReason, Fence, FenceLog, Membership, MembershipSource, Mri, PostReadDenyReason, Roster, Scope,
    Target, WriteKind,
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const TENANT: &str = "00000000-0000-4000-8000-000000000001";
const GUID_A: &str = "00000000-0000-4000-8000-00000000000a";
const GUID_B: &str = "00000000-0000-4000-8000-00000000000b";
const GUID_C: &str = "00000000-0000-4000-8000-00000000000c";
/// Someone outside the lab: the personal fence writes to a chat with them, which the lab fence never does.
const GUID_X: &str = "00000000-0000-4000-8000-0000000000ff";
const GROUP: &str = "19:0000000000000000000000000000abcd@thread.v2";

fn mri(guid: &str) -> Mri {
    Mri::new(format!("8:orgid:{guid}"))
}

fn one_to_one(first: &str, second: &str) -> String {
    format!("19:{first}_{second}@unq.gbl.spaces")
}

/// A fence log whose lines can be read back.
#[derive(Clone, Default)]
struct Lines(Arc<Mutex<Vec<u8>>>);

impl Write for Lines {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Lines {
    fn entries(&self) -> Vec<Value> {
        let bytes = self.0.lock().unwrap_or_else(PoisonError::into_inner).clone();
        String::from_utf8_lossy(&bytes)
            .lines()
            .map(|line| serde_json::from_str(line).unwrap_or(Value::Null))
            .collect()
    }
}

/// A log that takes nothing.
struct Broken;

impl Write for Broken {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("full"))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Err(std::io::Error::other("full"))
    }
}

/// The personal fence of account `guid`, and its log.
fn personal(guid: &str) -> (Fence, Lines) {
    let lines = Lines::default();
    (Fence::personal(mri(guid), FenceLog::from_writer(lines.clone())), lines)
}

/// A membership source with one fixed answer, counting how often it is asked.
struct Fixed(Membership, AtomicUsize);

impl Fixed {
    fn new(membership: Membership) -> Self {
        Self(membership, AtomicUsize::new(0))
    }

    fn reads(&self) -> usize {
        self.1.load(Ordering::Relaxed)
    }
}

impl MembershipSource for Fixed {
    fn members<'a>(&'a self, _: &'a Scope) -> teams_fence::MembershipFuture<'a> {
        self.1.fetch_add(1, Ordering::Relaxed);
        let answer = self.0.clone();
        Box::pin(async move { answer })
    }
}

fn message(id: &str) -> Target {
    Target::new(WriteKind::Message, Scope::Conversation { id: id.to_owned() })
}

/// Every target a write may name: chats of every kind (one the account isn't named in too), a channel, a team, a call,
/// a new target, the account itself and another account.
fn every_scope() -> Vec<Scope> {
    let conversation = |id: &str| Scope::Conversation { id: id.to_owned() };
    vec![
        conversation(GROUP),
        conversation(&one_to_one(GUID_A, GUID_X)),
        conversation(&one_to_one(GUID_B, GUID_X)),
        conversation("19:meeting_made-up@thread.v2"),
        conversation("48:notifications"),
        Scope::Channel {
            team: "19:made-up-team@thread.tacv2".into(),
            channel: "19:made-up-channel@thread.tacv2".into(),
        },
        Scope::Team {
            id: "19:made-up-team@thread.tacv2".into(),
        },
        Scope::Call { id: "made-up".into() },
        Scope::New,
        Scope::Account {
            mri: format!("8:orgid:{GUID_A}"),
        },
        Scope::Account {
            mri: format!("8:orgid:{GUID_B}"),
        },
    ]
}

#[tokio::test]
async fn the_personal_fence_permits_every_write_on_every_target_unread_and_logs_each() -> TestResult {
    let (fence, log) = personal(GUID_A);
    let source = Fixed::new(Membership::Complete(vec![mri(GUID_B)]));
    let mut checked = 0;
    for kind in WriteKind::ALL {
        for scope in every_scope() {
            let target = Target::new(kind, scope.clone()).with_added([mri(GUID_X)]);
            let permit = fence.check(target.clone(), &source).await;
            assert_eq!(
                permit.map(|permit| permit.target().clone()).ok(),
                Some(target),
                "{kind} on {scope}"
            );
            checked += 1;
        }
    }
    assert_eq!(source.reads(), 0, "the personal fence reads no members");
    let entries = log.entries();
    assert_eq!(entries.len(), checked, "every decision logged");
    assert!(entries.iter().all(|entry| entry["verdict"] == "allow"));
    Ok(())
}

#[tokio::test]
async fn a_channel_post_a_meeting_chat_and_a_new_chat_pass_where_the_lab_fence_denies() -> TestResult {
    let channel = Target::new(
        WriteKind::ChannelPost,
        Scope::Channel {
            team: "19:made-up-team@thread.tacv2".into(),
            channel: "19:made-up-channel@thread.tacv2".into(),
        },
    );
    let meeting = message("19:meeting_made-up@thread.v2");
    let new_chat = Target::new(WriteKind::ChatCreate, Scope::New).with_added([mri(GUID_A), mri(GUID_X)]);
    let outsider = message(&one_to_one(GUID_A, GUID_X));
    let (fence, _) = personal(GUID_A);
    let lab = Fence::new(
        Roster::new([mri(GUID_A), mri(GUID_B)], []),
        FenceLog::from_writer(std::io::sink()),
    );
    let members = Fixed::new(Membership::Complete(vec![mri(GUID_A), mri(GUID_X)]));
    for target in [channel, meeting, new_chat, outsider] {
        assert!(fence.check(target.clone(), &members).await.is_ok(), "{target}");
        let denied = lab.check(target.clone(), &members).await.unwrap_err();
        assert_eq!(denied.reason(), &DenyReason::Outsiders { count: 1 }, "{target}");
    }
    Ok(())
}

#[tokio::test]
async fn every_call_write_passes_a_call_of_this_run_or_not() -> TestResult {
    let (fence, log) = personal(GUID_A);
    let source = teams_fence::FailClosed;
    // a start in any chat, naming anyone, and a test call with anyone
    let start = Target::new(WriteKind::Call, Scope::Conversation { id: GROUP.to_owned() })
        .with_added([mri(GUID_A), mri(GUID_X)]);
    assert!(fence.check(start, &source).await.is_ok());
    let test_call = Target::new(WriteKind::Call, Scope::New).with_added([mri(GUID_A), mri(GUID_B), mri(GUID_X)]);
    assert!(fence.check(test_call, &source).await.is_ok());
    // operations on no call this run's fence registered
    let conversation = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/conv.example.test/conv/made-up";
    let operation = |operation: CallOperation, url: String, joins: Option<String>| {
        Target::new(
            operation.kind(),
            Scope::CallOperation {
                operation,
                url,
                from: Some(mri(GUID_A)),
                callbacks: Box::new([]),
                joins,
            },
        )
    };
    let leave = operation(CallOperation::Leave, format!("{conversation}/leave"), None);
    let add = operation(CallOperation::Add, format!("{conversation}/add"), None).with_added([mri(GUID_X)]);
    let join = operation(CallOperation::Join, conversation.to_owned(), Some(GROUP.to_owned()));
    for target in [leave, add, join] {
        assert!(fence.check(target.clone(), &source).await.is_ok(), "{target}");
    }
    assert!(log.entries().iter().all(|entry| entry["verdict"] == "allow"));
    assert_eq!(log.entries().len(), 5);
    Ok(())
}

#[tokio::test]
async fn an_allow_the_personal_log_can_t_take_isn_t_permitted() -> TestResult {
    let fence = Fence::personal(mri(GUID_A), FenceLog::from_writer(Broken));
    let denied = fence.check(message(GROUP), &teams_fence::FailClosed).await.unwrap_err();
    assert_eq!(denied.reason(), &DenyReason::LogUnavailable);
    let denied = fence.check_post_read("profile", &[mri(GUID_X)]).unwrap_err();
    assert_eq!(denied.reason(), PostReadDenyReason::LogUnavailable);
    assert_eq!(fence.own_account(), Some(&mri(GUID_A)));
    assert!(fence.is_personal());
    Ok(())
}

#[test]
fn a_personal_read_by_post_names_anyone_and_a_lab_one_keeps_its_rule() {
    let (fence, log) = personal(GUID_A);
    for names in [
        vec![mri(GUID_X)],
        vec![mri(GUID_A), mri(GUID_B), mri(GUID_C)],
        vec![Mri::new("28:00000000-0000-4000-8000-0000000000b0")],
        vec![],
        vec![mri(GUID_B), mri(GUID_B)],
    ] {
        let permit = fence.check_post_read("profile", &names);
        assert_eq!(permit.map(|permit| permit.names().to_vec()).ok(), Some(names));
    }
    assert_eq!(log.entries().len(), 5);
    let lab = Fence::new(Roster::new([mri(GUID_A)], []), FenceLog::from_writer(std::io::sink()));
    assert!(!lab.is_personal());
    assert_eq!(
        lab.check_post_read("profile", &[mri(GUID_X)]).unwrap_err().reason(),
        PostReadDenyReason::Outsiders { count: 1 }
    );
}

/// The paths of a personal build under `dir`.
fn paths_in(dir: &tempfile::TempDir) -> PersonalPaths {
    PersonalPaths::new(dir.path().join("config/teams"), dir.path().join("share/teams"))
}

/// Lab locations under `dir`, with a `lab.toml` naming lab accounts a, b and c when `text` is given.
fn lab_in(dir: &tempfile::TempDir, text: Option<&str>) -> TestResult<LabLocations> {
    let config_file = dir.path().join("config/teams-lab/lab.toml");
    if let Some(text) = text {
        fs::create_dir_all(config_file.parent().ok_or("dir")?)?;
        fs::write(&config_file, text)?;
    }
    Ok(LabLocations {
        config_file,
        data_dir: dir.path().join("share/teams-lab"),
        personal_data_dir: dir.path().join("share/teams"),
    })
}

fn lab_toml() -> String {
    let mut text = String::new();
    for (alias, guid) in ["a", "b", "c"].iter().zip([GUID_A, GUID_B, GUID_C]) {
        text.push_str(
            &[
                "[[accounts]]\nalias = \"",
                alias,
                "\"\nupn = \"bot-",
                alias,
                "@lab.example.test\"\n",
            ]
            .concat(),
        );
        text.push_str(&["mri = \"8:orgid:", guid, "\"\n"].concat());
    }
    text
}

#[test]
fn the_personal_fence_opens_only_with_an_account_apart_from_the_lab() -> TestResult {
    let dir = tempfile::tempdir()?;
    let paths = paths_in(&dir);
    let lab = lab_in(&dir, Some(&lab_toml()))?;
    let someone = "someone@example.test";
    // before the first sign-in there is no account, or one without its object id
    assert!(matches!(
        Fence::open_personal_with(&paths, someone, &lab),
        Err(PersonalError::NoAccount)
    ));
    PersonalAccount::pending(someone)?.save(&paths)?;
    assert!(matches!(
        Fence::open_personal_with(&paths, someone, &lab),
        Err(PersonalError::NoAccount)
    ));
    // a lab account, by UPN or by MRI, is refused
    for (upn, oid) in [("BOT-A@lab.example.test", GUID_X), (someone, GUID_B)] {
        PersonalAccount::new(upn, oid, TENANT)?.save(&paths)?;
        assert!(matches!(
            Fence::open_personal_with(&paths, upn, &lab),
            Err(PersonalError::LabAccount)
        ));
        PersonalAccount::remove(&paths, upn)?;
    }
    let account = PersonalAccount::new(someone, GUID_X, TENANT)?;
    account.save(&paths)?;
    let fence = Fence::open_personal_with(&paths, someone, &lab)?;
    assert_eq!(fence.own_account(), Some(&mri(GUID_X)));
    assert!(fence.is_personal());
    assert!(paths.fence_log().is_file());
    assert_eq!(fs::metadata(paths.accounts_file())?.permissions().mode() & 0o777, 0o600);
    assert_eq!(fs::metadata(paths.config_dir())?.permissions().mode() & 0o777, 0o700);
    assert_eq!(fs::metadata(paths.data_dir())?.permissions().mode() & 0o777, 0o700);
    assert_eq!(PersonalAccount::load_all(&paths)?, vec![account]);
    Ok(())
}

/// A made-up account `upn` a sign-in named, with the object id `oid`.
fn signed_in(upn: &str, oid: &str) -> TestResult<PersonalAccount> {
    Ok(PersonalAccount::new(upn, oid, TENANT)?)
}

/// The UPNs of `paths`' accounts, in their order.
fn upns(paths: &PersonalPaths) -> TestResult<Vec<String>> {
    Ok(PersonalAccount::load_all(paths)?
        .into_iter()
        .map(|account| account.upn)
        .collect())
}

const GUID_Y: &str = "00000000-0000-4000-8000-0000000000fe";

#[test]
fn the_personal_fence_opens_for_each_signed_in_account_by_its_upn() -> TestResult {
    let dir = tempfile::tempdir()?;
    let paths = paths_in(&dir);
    let lab = lab_in(&dir, Some(&lab_toml()))?;
    signed_in("one@example.test", GUID_X)?.save(&paths)?;
    signed_in("two@example.test", GUID_Y)?.save(&paths)?;
    PersonalAccount::pending("three@example.test")?.save(&paths)?;
    for (upn, guid) in [("one@example.test", GUID_X), ("TWO@Example.Test", GUID_Y)] {
        let fence = Fence::open_personal_with(&paths, upn, &lab)?;
        assert_eq!(fence.own_account(), Some(&mri(guid)), "{upn}");
    }
    // one that isn't an account, and one no sign-in named yet
    for upn in ["four@example.test", "three@example.test"] {
        assert!(
            matches!(
                Fence::open_personal_with(&paths, upn, &lab),
                Err(PersonalError::NoAccount)
            ),
            "{upn}"
        );
    }
    Ok(())
}

#[test]
fn accounts_toml_keeps_the_accounts_in_the_order_added_and_saves_by_upn() -> TestResult {
    let dir = tempfile::tempdir()?;
    let paths = paths_in(&dir);
    assert!(PersonalAccount::load_all(&paths)?.is_empty(), "none before the first");
    PersonalAccount::pending("one@example.test")?.save(&paths)?;
    signed_in("two@example.test", GUID_X)?.save(&paths)?;
    PersonalAccount::pending("three@example.test")?.save(&paths)?;
    assert_eq!(
        upns(&paths)?,
        ["one@example.test", "two@example.test", "three@example.test"]
    );
    // a sign-in of the first, its UPN in another case: saved in its place
    let one = signed_in("ONE@example.test", GUID_Y)?;
    one.save(&paths)?;
    assert_eq!(
        upns(&paths)?,
        ["ONE@example.test", "two@example.test", "three@example.test"]
    );
    assert_eq!(PersonalAccount::find(&paths, "one@EXAMPLE.test")?, Some(one));
    assert_eq!(PersonalAccount::find(&paths, "four@example.test")?, None);
    // another account with an object id one has already: refused, nothing written
    let before = fs::read_to_string(paths.accounts_file())?;
    assert!(matches!(
        signed_in("four@example.test", GUID_X)?.save(&paths),
        Err(PersonalError::Account(_))
    ));
    assert_eq!(fs::read_to_string(paths.accounts_file())?, before);
    // removed by UPN (case aside), the others kept in their order
    let removed = PersonalAccount::remove(&paths, "Two@Example.Test")?;
    assert_eq!(removed.map(|account| account.oid), Some(Some(GUID_X.to_owned())));
    assert_eq!(upns(&paths)?, ["ONE@example.test", "three@example.test"]);
    assert_eq!(PersonalAccount::remove(&paths, "two@example.test")?, None);
    // the file and its directory are the user's alone, and no staging file is left
    assert_eq!(fs::metadata(paths.accounts_file())?.permissions().mode() & 0o777, 0o600);
    assert_eq!(fs::metadata(paths.config_dir())?.permissions().mode() & 0o777, 0o700);
    let names: Vec<_> = fs::read_dir(paths.config_dir())?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<_, _>>()?;
    assert_eq!(names, ["accounts.toml"]);
    Ok(())
}

#[test]
fn accounts_toml_refuses_an_account_given_twice_or_one_that_isn_t_valid() -> TestResult {
    let dir = tempfile::tempdir()?;
    let paths = paths_in(&dir);
    fs::create_dir_all(paths.config_dir())?;
    let table = |upn: &str, oid: &str| format!("[[account]]\nupn = \"{upn}\"\noid = \"{oid}\"\ntid = \"{TENANT}\"\n");
    let good = format!(
        "{}\n{}",
        table("one@example.test", GUID_X),
        table("two@example.test", GUID_Y)
    );
    fs::write(paths.accounts_file(), &good)?;
    assert_eq!(upns(&paths)?, ["one@example.test", "two@example.test"]);
    for bad in [
        // the same UPN, case aside
        format!(
            "{}\n{}",
            table("one@example.test", GUID_X),
            table("ONE@example.test", GUID_Y)
        ),
        // the same object id, case aside
        format!(
            "{}\n{}",
            table("one@example.test", GUID_X),
            table("two@example.test", &GUID_X.to_ascii_uppercase())
        ),
        // an account that isn't valid, an unknown key in one, an unknown table
        format!("{good}\n{}", table("three", GUID_C)),
        format!("{good}token = \"x\"\n"),
        format!("{good}\n[settings]\nx = 1\n"),
    ] {
        fs::write(paths.accounts_file(), &bad)?;
        assert!(
            matches!(PersonalAccount::load_all(&paths), Err(PersonalError::Account(_))),
            "{bad}"
        );
        // nothing is saved over a file that doesn't read
        assert!(PersonalAccount::pending("four@example.test")?.save(&paths).is_err());
        assert_eq!(fs::read_to_string(paths.accounts_file())?, bad);
    }
    // a file of no account is none
    fs::write(paths.accounts_file(), "# no account yet\n")?;
    assert!(PersonalAccount::load_all(&paths)?.is_empty());
    Ok(())
}

#[test]
fn a_single_account_s_files_are_read_until_moved_to_the_files_of_several() -> TestResult {
    let dir = tempfile::tempdir()?;
    let paths = paths_in(&dir);
    paths.make_dirs()?;
    let legacy =
        format!("upn = \"someone@example.test\"\noid = \"{GUID_X}\"\ntid = \"{TENANT}\"\nname = \"Made-Up\"\n");
    fs::write(paths.legacy_account_file(), &legacy)?;
    fs::write(paths.legacy_region_file(), "{\"region\":\"amer\"}")?;
    let account = PersonalAccount::parse(&legacy)?;
    // not migrated yet: the one account is read from account.toml
    assert_eq!(PersonalAccount::load_all(&paths)?, vec![account.clone()]);
    assert_eq!(
        PersonalAccount::find(&paths, "SOMEONE@example.test")?,
        Some(account.clone())
    );
    paths.migrate_single_account()?;
    assert!(!paths.legacy_account_file().exists());
    assert!(!paths.legacy_region_file().exists());
    let region = paths
        .region_file(&account)
        .ok_or("a signed-in account has a region file")?;
    assert_eq!(region, paths.data_dir().join("regions").join(format!("{GUID_X}.json")));
    assert_eq!(fs::read_to_string(&region)?, "{\"region\":\"amer\"}");
    assert_eq!(
        fs::metadata(region.parent().ok_or("dir")?)?.permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(PersonalAccount::load_all(&paths)?, vec![account.clone()]);
    assert_eq!(fs::metadata(paths.accounts_file())?.permissions().mode() & 0o777, 0o600);
    // a second time: nothing to do
    let migrated = fs::read_to_string(paths.accounts_file())?;
    paths.migrate_single_account()?;
    assert_eq!(fs::read_to_string(paths.accounts_file())?, migrated);
    // an account.toml beside an accounts.toml (an older build run since) is left alone
    fs::write(paths.legacy_account_file(), "upn = \"other@example.test\"\n")?;
    fs::write(paths.legacy_region_file(), "{}")?;
    paths.migrate_single_account()?;
    assert!(paths.legacy_account_file().exists() && paths.legacy_region_file().exists());
    assert_eq!(fs::read_to_string(paths.accounts_file())?, migrated);
    assert_eq!(PersonalAccount::load_all(&paths)?, vec![account]);
    Ok(())
}

#[test]
fn a_single_account_no_sign_in_named_moves_without_its_region() -> TestResult {
    let dir = tempfile::tempdir()?;
    let paths = paths_in(&dir);
    // nothing to move: no file is made
    paths.migrate_single_account()?;
    assert!(!paths.accounts_file().exists());
    paths.make_dirs()?;
    fs::write(paths.legacy_account_file(), "upn = \"someone@example.test\"\n")?;
    fs::write(paths.legacy_region_file(), "{}")?;
    paths.migrate_single_account()?;
    let pending = PersonalAccount::pending("someone@example.test")?;
    assert_eq!(PersonalAccount::load_all(&paths)?, vec![pending.clone()]);
    assert_eq!(paths.region_file(&pending), None);
    assert!(!paths.legacy_account_file().exists());
    assert!(
        paths.legacy_region_file().exists(),
        "the next sign-in writes its region"
    );
    // an account.toml that doesn't read isn't moved
    let dir = tempfile::tempdir()?;
    let paths = paths_in(&dir);
    paths.make_dirs()?;
    fs::write(paths.legacy_account_file(), "upn = \"someone\"\n")?;
    assert!(paths.migrate_single_account().is_err());
    assert!(paths.legacy_account_file().exists() && !paths.accounts_file().exists());
    Ok(())
}

#[test]
fn a_personal_data_dir_overlapping_the_lab_s_is_refused() -> TestResult {
    let dir = tempfile::tempdir()?;
    let lab = lab_in(&dir, None)?;
    for data in [
        dir.path().join("share/teams-lab"),
        dir.path().join("share/teams-lab/teams"),
        dir.path().join("share"),
    ] {
        let paths = PersonalPaths::new(dir.path().join("config/teams"), data.clone());
        assert!(
            matches!(paths.check_apart(&lab), Err(PersonalError::LabDataDir { .. })),
            "{data:?}"
        );
    }
    // lab.toml's data_dir too
    let elsewhere = dir.path().join("share/teams");
    let lab = lab_in(&dir, Some(&format!("data_dir = {elsewhere:?}\n{}", lab_toml())))?;
    assert!(matches!(
        paths_in(&dir).check_apart(&lab),
        Err(PersonalError::LabDataDir { .. })
    ));
    Ok(())
}

#[test]
fn account_toml_is_strict() {
    let good = format!("upn = \"someone@example.test\"\noid = \"{GUID_X}\"\ntid = \"{TENANT}\"\n");
    assert!(PersonalAccount::parse(&good).is_ok());
    let upper = good.replace(GUID_X, &GUID_X.to_ascii_uppercase());
    assert_eq!(
        PersonalAccount::parse(&upper).map(|account| account.oid).ok(),
        Some(Some(GUID_X.to_owned()))
    );
    // before the first sign-in: the UPN alone
    let pending = PersonalAccount::parse("upn = \"someone@example.test\"\n").unwrap();
    assert!(!pending.is_signed_in() && pending.mri().is_none());
    for bad in [
        good.replace("someone@example.test", "someone"),
        good.replace(GUID_X, "not-a-guid"),
        good.replace(&format!("tid = \"{TENANT}\"\n"), ""),
        format!("{good}token = \"x\"\n"),
        format!("{good}idp_hosts = [\"Bad Host\"]\n"),
        format!("{good}idp_hosts = [\"localhost\"]\n"),
    ] {
        assert!(PersonalAccount::parse(&bad).is_err(), "{bad}");
    }
    for bad_name in ["", " padded", "line\\nbreak"] {
        let bad = format!("{good}name = \"{bad_name}\"\n");
        assert!(PersonalAccount::parse(&bad).is_err(), "{bad}");
    }
    let hosts = format!("{good}idp_hosts = [\"sso.example.test\"]\n");
    assert_eq!(
        PersonalAccount::parse(&hosts).map(|account| account.idp_hosts).ok(),
        Some(vec!["sso.example.test".to_owned()])
    );
}

#[test]
fn a_sign_in_s_display_name_is_kept_in_accounts_toml() -> TestResult {
    let dir = tempfile::tempdir()?;
    let paths = paths_in(&dir);
    let pending = PersonalAccount::pending("someone@example.test")?;
    let named = pending.signed_in_as(GUID_X, TENANT, Some("Made-Up Person"))?;
    assert_eq!(named.name.as_deref(), Some("Made-Up Person"));
    named.save(&paths)?;
    assert_eq!(PersonalAccount::load_all(&paths)?, vec![named.clone()]);
    // a later sign-in without one, or with one a post can't carry, keeps it
    let again = named.signed_in_as(GUID_X, TENANT, None)?;
    assert_eq!(again.name.as_deref(), Some("Made-Up Person"));
    let again = named.signed_in_as(GUID_X, TENANT, Some("bad\u{7}name"))?;
    assert_eq!(again.name.as_deref(), Some("Made-Up Person"));
    // the store is the account's own
    assert_eq!(
        paths.store_file(&named),
        Some(paths.data_dir().join("store").join(format!("{GUID_X}.sqlite")))
    );
    assert_eq!(paths.store_file(&pending), None);
    Ok(())
}
