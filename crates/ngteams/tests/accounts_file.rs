//! Account-file parsing, persistence, permissions and legacy migration, using synthetic identities.

use ngteams::accounts_file::{PersonalAccount, PersonalError, PersonalPaths};
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const TENANT: &str = "00000000-0000-4000-8000-000000000001";
const GUID_C: &str = "00000000-0000-4000-8000-00000000000c";
const GUID_X: &str = "00000000-0000-4000-8000-0000000000ff";
const GUID_Y: &str = "00000000-0000-4000-8000-0000000000fe";
fn paths_in(dir: &tempfile::TempDir) -> PersonalPaths {
    PersonalPaths::new(dir.path().join("config/ngteams"), dir.path().join("share/ngteams"))
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

#[test]
fn an_unwritable_config_reports_the_target_and_preserves_published_accounts() -> TestResult {
    let dir = tempfile::tempdir()?;
    let paths = paths_in(&dir);
    PersonalAccount::pending("first@example.test")?.save(&paths)?;
    let before = fs::read(paths.accounts_file())?;
    let mode = fs::metadata(paths.config_dir())?.permissions();
    fs::set_permissions(paths.config_dir(), fs::Permissions::from_mode(0o555))?;
    let result = PersonalAccount::pending("second@example.test")?.save(&paths);
    let after = fs::read(paths.accounts_file());
    // Restore before assertions, including on the expected refusal, so the private fixture cleans up normally.
    fs::set_permissions(paths.config_dir(), mode)?;
    assert!(matches!(result, Err(PersonalError::Write { path, .. }) if path == paths.accounts_file()));
    assert_eq!(after?, before);
    assert_eq!(upns(&paths)?, ["first@example.test"]);
    Ok(())
}

#[test]
fn account_file_obstructions_are_read_errors_instead_of_empty_account_lists() -> TestResult {
    let dir = tempfile::tempdir()?;
    let paths = paths_in(&dir);
    fs::create_dir_all(paths.accounts_file())?;
    assert!(
        matches!(PersonalAccount::load_all(&paths), Err(PersonalError::Read { path, .. }) if path == paths.accounts_file())
    );
    fs::remove_dir(paths.accounts_file())?;
    fs::create_dir(paths.legacy_account_file())?;
    assert!(
        matches!(PersonalAccount::load_all(&paths), Err(PersonalError::Read { path, .. }) if path == paths.legacy_account_file())
    );
    Ok(())
}
