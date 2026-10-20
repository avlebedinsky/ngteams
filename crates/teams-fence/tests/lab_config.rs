//! Parsing and validating `lab.toml`: exactly the 3 lab accounts `a`, `b`, `c`, all in one tenant domain, plus the
//! approved service participants and the lab directories.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use std::os::unix::fs::PermissionsExt as _;

use teams_fence::lab::{
    Alias, ChatRecord, ChatsError, ConfigError, LabChats, LabConfig, LabLocations, LabPaths, MriRecord,
    record_abc_chat, record_account_mri, record_service_mri,
};

const DATA: &str = "/lab/data";

fn account(alias: &str, upn: &str) -> String {
    format!("[[accounts]]\nalias = \"{alias}\"\nupn = \"{upn}\"\n\n")
}

fn three() -> String {
    [
        account("a", "bot-a@lab.example.com"),
        account("b", "bot-b@lab.example.com"),
        account("c", "bot-c@lab.example.com"),
    ]
    .concat()
}

fn parse(text: &str) -> Result<LabConfig, ConfigError> {
    LabConfig::parse(text, Path::new(DATA))
}

#[test]
fn three_accounts_in_one_domain_parse() {
    let text = format!(
        "{}[[accounts]]\nalias = \"b\"\nupn = \"Bot-B@Lab.Example.com\"\nmri = \"<mri:b>\"\n\n{}\
         [[service_participants]]\nname = \"echo\"\n",
        account("c", "bot-c@lab.example.com"),
        account("a", "bot-a@lab.example.com"),
    );
    let config = parse(&text).unwrap();

    let aliases: Vec<Alias> = config.accounts().iter().map(|account| account.alias).collect();
    assert_eq!(
        aliases,
        [Alias::A, Alias::B, Alias::C],
        "accounts are kept in alias order"
    );
    assert_eq!(config.account(Alias::B).upn, "Bot-B@Lab.Example.com");
    assert_eq!(config.account(Alias::B).mri.as_deref(), Some("<mri:b>"));
    assert_eq!(
        config.account(Alias::A).mri,
        None,
        "MRIs are filled in after the first login"
    );
    assert_eq!(config.tenant_domain(), "lab.example.com");

    assert_eq!(config.service_participants().len(), 1);
    assert_eq!(config.service_participants()[0].name, "echo");
    assert_eq!(config.service_participants()[0].mri, None);
}

#[test]
fn lab_directories_derive_from_the_data_dir() {
    let config = parse(&three()).unwrap();
    let paths = config.paths();
    let data = PathBuf::from(DATA);
    assert_eq!(paths.data_dir(), data);
    assert_eq!(paths.chrome_dir(), data.join("chrome"));
    assert_eq!(paths.profiles_dir(), data.join("profiles"));
    assert_eq!(paths.profile_dir(Alias::C), data.join("profiles/c"));
    assert_eq!(paths.captures_dir(), data.join("captures"));
    assert_eq!(paths.bundles_dir(), data.join("bundles"));
    assert_eq!(paths.fence_log(), data.join("fence.log"));
    assert_eq!(paths.region_file(Alias::B), data.join("auth/b.json"));
}

#[test]
fn the_lab_s_secrets_live_under_its_own_keyring_application() {
    assert_eq!(LabPaths::KEYRING_APPLICATION, "teams-lab");
}

#[test]
fn data_dir_in_the_file_overrides_the_default() {
    let config = parse(&format!("data_dir = \"/elsewhere/lab\"\n\n{}", three())).unwrap();
    assert_eq!(config.paths().data_dir(), Path::new("/elsewhere/lab"));
}

#[test]
fn a_relative_data_dir_is_refused() {
    let error = parse(&format!("data_dir = \"lab\"\n\n{}", three())).unwrap_err();
    assert!(matches!(error, ConfigError::RelativeDataDir(_)), "{error:?}");
}

#[test]
fn any_count_but_three_is_refused() {
    let cases = [
        (String::new(), 0),
        (
            [
                account("a", "bot-a@lab.example.com"),
                account("b", "bot-b@lab.example.com"),
            ]
            .concat(),
            2,
        ),
        (format!("{}{}", three(), account("a", "bot-d@lab.example.com")), 4),
    ];
    for (text, count) in cases {
        let error = parse(&text).unwrap_err();
        assert!(
            matches!(error, ConfigError::AccountCount(n) if n == count),
            "{count}: {error:?}"
        );
        assert!(error.to_string().contains("exactly 3"), "{error}");
    }
}

#[test]
fn a_duplicate_alias_is_refused() {
    let text = [
        account("a", "bot-a@lab.example.com"),
        account("b", "bot-b@lab.example.com"),
        account("a", "bot-c@lab.example.com"),
    ]
    .concat();
    let error = parse(&text).unwrap_err();
    assert!(matches!(error, ConfigError::DuplicateAlias(Alias::A)), "{error:?}");
}

#[test]
fn a_duplicate_upn_is_refused_whatever_its_case() {
    let text = [
        account("a", "bot-a@lab.example.com"),
        account("b", "BOT-A@lab.example.com"),
        account("c", "bot-c@lab.example.com"),
    ]
    .concat();
    let error = parse(&text).unwrap_err();
    assert!(matches!(error, ConfigError::DuplicateUpn(_)), "{error:?}");
}

#[test]
fn a_mixed_domain_is_refused() {
    let text = [
        account("a", "bot-a@lab.example.com"),
        account("b", "bot-b@lab.example.com"),
        account("c", "bot-c@other.example.com"),
    ]
    .concat();
    let error = parse(&text).unwrap_err();
    match error {
        ConfigError::MixedDomain { expected, found } => {
            assert_eq!(expected, "lab.example.com");
            assert_eq!(found, "other.example.com");
        }
        other => panic!("expected MixedDomain, got {other:?}"),
    }
}

#[test]
fn a_malformed_upn_is_refused() {
    for upn in [
        "bot-c",
        "@lab.example.com",
        "bot-c@",
        "bot@c@lab.example.com",
        "bot c@lab.example.com",
    ] {
        let text = [
            account("a", "bot-a@lab.example.com"),
            account("b", "bot-b@lab.example.com"),
            account("c", upn),
        ]
        .concat();
        let error = parse(&text).unwrap_err();
        assert!(matches!(error, ConfigError::InvalidUpn(_)), "{upn}: {error:?}");
    }
}

#[test]
fn an_unknown_alias_or_field_is_a_parse_error() {
    let unknown_alias = [
        account("a", "bot-a@lab.example.com"),
        account("b", "bot-b@lab.example.com"),
        account("d", "bot-d@lab.example.com"),
    ]
    .concat();
    assert!(matches!(parse(&unknown_alias).unwrap_err(), ConfigError::Parse(_)));

    let typo = format!("{}[[service_participants]]\nname = \"echo\"\nmrl = \"x\"\n", three());
    assert!(matches!(parse(&typo).unwrap_err(), ConfigError::Parse(_)));
}

#[test]
fn an_empty_mri_is_refused() {
    let text = format!(
        "{}{}[[accounts]]\nalias = \"c\"\nupn = \"bot-c@lab.example.com\"\nmri = \"\"\n",
        account("a", "bot-a@lab.example.com"),
        account("b", "bot-b@lab.example.com"),
    );
    assert!(matches!(parse(&text).unwrap_err(), ConfigError::EmptyMri(_)));

    let service = format!("{}[[service_participants]]\nname = \"echo\"\nmri = \"\"\n", three());
    assert!(matches!(parse(&service).unwrap_err(), ConfigError::EmptyMri(_)));
}

#[test]
fn one_mri_on_two_participants_is_refused() {
    let twice = format!(
        "{}[[accounts]]\nalias = \"c\"\nupn = \"bot-c@lab.example.com\"\nmri = \"<mri:x>\"\n\n\
         [[accounts]]\nalias = \"b\"\nupn = \"bot-b@lab.example.com\"\nmri = \"<mri:x>\"\n",
        account("a", "bot-a@lab.example.com"),
    );
    assert!(matches!(parse(&twice).unwrap_err(), ConfigError::DuplicateMri(_)));

    let service = format!(
        "{}{}[[accounts]]\nalias = \"c\"\nupn = \"bot-c@lab.example.com\"\nmri = \"<mri:x>\"\n\n\
         [[service_participants]]\nname = \"echo\"\nmri = \"<mri:x>\"\n",
        account("a", "bot-a@lab.example.com"),
        account("b", "bot-b@lab.example.com"),
    );
    assert!(matches!(parse(&service).unwrap_err(), ConfigError::DuplicateMri(_)));
}

#[test]
fn a_duplicate_or_empty_service_name_is_refused() {
    let duplicate = format!(
        "{}[[service_participants]]\nname = \"echo\"\n\n[[service_participants]]\nname = \"echo\"\n",
        three()
    );
    assert!(matches!(
        parse(&duplicate).unwrap_err(),
        ConfigError::DuplicateService(_)
    ));

    let empty = format!("{}[[service_participants]]\nname = \"\"\n", three());
    assert!(matches!(parse(&empty).unwrap_err(), ConfigError::EmptyServiceName));
}

#[test]
fn aliases_round_trip_through_text() {
    for alias in Alias::ALL {
        assert_eq!(alias.to_string().parse::<Alias>().unwrap(), alias);
    }
    assert!("d".parse::<Alias>().is_err());
    assert!("A".parse::<Alias>().is_err());
}

#[test]
fn load_reads_the_file_and_names_it_on_error() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("lab.toml");
    fs::write(&file, three()).unwrap();
    let locations = LabLocations {
        config_file: file.clone(),
        data_dir: dir.path().join("data"),
    };
    let config = LabConfig::load(&locations).unwrap();
    assert_eq!(config.paths().data_dir(), dir.path().join("data"));

    fs::remove_file(&file).unwrap();
    let error = LabConfig::load(&locations).unwrap_err();
    assert!(matches!(error, ConfigError::Read { .. }), "{error:?}");
    assert!(error.to_string().contains("lab.toml"), "{error}");
}

#[test]
fn lab_paths_follow_lab_toml_and_need_none() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("lab.toml");
    let locations = LabLocations {
        config_file: file.clone(),
        data_dir: dir.path().join("default"),
    };

    // no lab.toml: the default data dir
    assert_eq!(
        locations.lab_paths().unwrap().chrome_dir(),
        dir.path().join("default/chrome")
    );

    // the file's data_dir wins, as for every command that loads the config
    let elsewhere = dir.path().join("elsewhere");
    fs::write(&file, format!("data_dir = {elsewhere:?}\n{}", three())).unwrap();
    assert_eq!(locations.lab_paths().unwrap().chrome_dir(), elsewhere.join("chrome"));
    assert_eq!(
        locations.lab_paths().unwrap(),
        LabConfig::load(&locations).unwrap().paths().clone()
    );

    // a broken lab.toml isn't skipped over
    fs::write(&file, format!("data_dir = {elsewhere:?}\n")).unwrap();
    assert!(matches!(locations.lab_paths(), Err(ConfigError::AccountCount(0))));
}

fn os(value: &str) -> OsString {
    OsString::from(value)
}

#[test]
fn locations_follow_xdg_and_fall_back_to_home() {
    let xdg = LabLocations::resolve(Some(os("/xdg/config")), Some(os("/xdg/data")), Some(os("/home/u"))).unwrap();
    assert_eq!(xdg.config_file, Path::new("/xdg/config/teams-lab/lab.toml"));
    assert_eq!(xdg.data_dir, Path::new("/xdg/data/teams-lab"));

    let home = LabLocations::resolve(None, None, Some(os("/home/u"))).unwrap();
    assert_eq!(home.config_file, Path::new("/home/u/.config/teams-lab/lab.toml"));
    assert_eq!(home.data_dir, Path::new("/home/u/.local/share/teams-lab"));

    // The XDG spec says empty and relative values are ignored.
    let ignored = LabLocations::resolve(Some(os("")), Some(os("relative")), Some(os("/home/u"))).unwrap();
    assert_eq!(ignored, home);

    assert!(matches!(
        LabLocations::resolve(None, None, None),
        Err(ConfigError::NoHome)
    ));
    assert!(matches!(
        LabLocations::resolve(None, None, Some(os("home"))),
        Err(ConfigError::NoHome)
    ));
}

#[test]
#[ignore = "reads this workstation's ~/.config/teams-lab/lab.toml; run with --ignored"]
fn the_real_lab_config_is_valid() {
    let config = LabConfig::load(&LabLocations::from_env().unwrap()).unwrap();
    assert!(
        config
            .service_participants()
            .iter()
            .any(|service| service.name == "echo")
    );
}

// --- recording an account's MRI after its first login ---

const COMMENTED: &str = "# The lab accounts. Passwords never go here.\n\
data_dir = \"/lab/data\"\n\n\
[[accounts]]\nalias = \"a\"\nupn = \"bot-a@lab.example.com\"\n# mri = \"\"\n\n\
[[accounts]]\nalias = \"b\"\nupn = \"bot-b@lab.example.com\"\n# filled in after login\n\n\
[[accounts]]\nalias = \"c\"\nupn = \"bot-c@lab.example.com\"\nmri = \"<mri:c>\"\n\n\
# approved by the user\n[[service_participants]]\nname = \"echo\"\n";

/// A private lab.toml holding `text`, and where to find it.
fn lab_file(text: &str) -> std::io::Result<(tempfile::TempDir, LabLocations)> {
    let dir = tempfile::tempdir()?;
    let file = dir.path().join("lab.toml");
    fs::write(&file, text)?;
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600))?;
    let locations = LabLocations {
        config_file: file,
        data_dir: dir.path().join("data"),
    };
    Ok((dir, locations))
}

fn names_in(dir: &Path) -> std::io::Result<Vec<String>> {
    let mut names = fs::read_dir(dir)?
        .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
        .collect::<std::io::Result<Vec<String>>>()?;
    names.sort();
    Ok(names)
}

#[test]
fn an_mri_is_recorded_for_one_account_keeping_the_rest_of_the_file() {
    let (dir, locations) = lab_file(COMMENTED).unwrap();
    let outcome = record_account_mri(&locations, Alias::A, "<mri:a>").unwrap();
    assert_eq!(outcome, MriRecord::Recorded);

    let text = fs::read_to_string(&locations.config_file).unwrap();
    for kept in [
        "# The lab accounts. Passwords never go here.",
        "# filled in after login",
        "# approved by the user",
        "data_dir = \"/lab/data\"",
        "mri = \"<mri:c>\"",
    ] {
        assert!(text.contains(kept), "lost {kept:?}:\n{text}");
    }
    let config = LabConfig::load(&locations).unwrap();
    assert_eq!(config.account(Alias::A).mri.as_deref(), Some("<mri:a>"));
    assert_eq!(config.account(Alias::B).mri, None);
    assert_eq!(config.account(Alias::C).mri.as_deref(), Some("<mri:c>"));
    let mode = fs::metadata(&locations.config_file).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "the file's mode is kept");
    assert_eq!(names_in(dir.path()).unwrap(), ["lab.toml"], "no temporary file is left");
}

#[test]
fn recording_the_same_mri_again_changes_nothing() {
    let (_dir, locations) = lab_file(COMMENTED).unwrap();
    let before = fs::read_to_string(&locations.config_file).unwrap();
    assert_eq!(
        record_account_mri(&locations, Alias::C, "<mri:c>").unwrap(),
        MriRecord::AlreadyRecorded
    );
    assert_eq!(fs::read_to_string(&locations.config_file).unwrap(), before);
}

#[test]
fn another_mri_already_recorded_or_taken_is_refused_and_nothing_is_written() {
    let (dir, locations) = lab_file(COMMENTED).unwrap();
    let before = fs::read_to_string(&locations.config_file).unwrap();

    let conflict = record_account_mri(&locations, Alias::C, "<mri:other>").unwrap_err();
    assert!(matches!(conflict, ConfigError::MriConflict(Alias::C)), "{conflict:?}");
    // the MRI of c can't become a's too
    let taken = record_account_mri(&locations, Alias::A, "<mri:c>").unwrap_err();
    assert!(matches!(taken, ConfigError::DuplicateMri(_)), "{taken:?}");
    let empty = record_account_mri(&locations, Alias::A, " ").unwrap_err();
    assert!(matches!(empty, ConfigError::EmptyMri(_)), "{empty:?}");

    assert_eq!(fs::read_to_string(&locations.config_file).unwrap(), before);
    assert_eq!(names_in(dir.path()).unwrap(), ["lab.toml"]);
}

#[test]
fn an_invalid_or_missing_file_gets_nothing_recorded() {
    let (dir, locations) = lab_file("[[accounts]]\nalias = \"a\"\nupn = \"bot-a@lab.example.com\"\n").unwrap();
    let error = record_account_mri(&locations, Alias::A, "<mri:a>").unwrap_err();
    assert!(matches!(error, ConfigError::AccountCount(1)), "{error:?}");
    assert_eq!(names_in(dir.path()).unwrap(), ["lab.toml"]);

    fs::remove_file(&locations.config_file).unwrap();
    let missing = record_account_mri(&locations, Alias::A, "<mri:a>").unwrap_err();
    assert!(matches!(missing, ConfigError::Read { .. }), "{missing:?}");
    assert!(names_in(dir.path()).unwrap().is_empty());
}

// --- recording an approved service participant's MRI ---

/// A made-up bot MRI.
const BOT: &str = "28:00000000-0000-4000-8000-0000000000e0";

#[test]
fn a_service_mri_is_recorded_for_the_named_service_keeping_the_rest_of_the_file() {
    let (dir, locations) = lab_file(COMMENTED).unwrap();
    assert_eq!(
        record_service_mri(&locations, "echo", BOT).unwrap(),
        MriRecord::Recorded
    );

    let text = fs::read_to_string(&locations.config_file).unwrap();
    for kept in [
        "# The lab accounts. Passwords never go here.",
        "# approved by the user",
        "mri = \"<mri:c>\"",
    ] {
        assert!(text.contains(kept), "lost {kept:?}:\n{text}");
    }
    let config = LabConfig::load(&locations).unwrap();
    let echo = config
        .service_participants()
        .iter()
        .find(|service| service.name == "echo")
        .unwrap();
    assert_eq!(echo.mri.as_deref(), Some(BOT));
    // no account changed
    assert_eq!(config.account(Alias::A).mri, None);
    let mode = fs::metadata(&locations.config_file).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "the file's mode is kept");
    assert_eq!(names_in(dir.path()).unwrap(), ["lab.toml"], "no temporary file is left");

    // the same again changes nothing
    let before = fs::read_to_string(&locations.config_file).unwrap();
    assert_eq!(
        record_service_mri(&locations, "echo", BOT).unwrap(),
        MriRecord::AlreadyRecorded
    );
    assert_eq!(fs::read_to_string(&locations.config_file).unwrap(), before);
}

#[test]
fn a_service_mri_is_refused_for_an_unknown_service_another_mri_or_one_not_a_bot_s() {
    let (dir, locations) = lab_file(COMMENTED).unwrap();
    let before = fs::read_to_string(&locations.config_file).unwrap();

    // only a service the user approved, by its name in lab.toml
    let unknown = record_service_mri(&locations, "other", BOT).unwrap_err();
    assert!(
        matches!(unknown, ConfigError::UnknownService(ref name) if name == "other"),
        "{unknown:?}"
    );
    // only a bot's MRI: `28:` and a lowercase GUID, nothing else
    for mri in [
        "8:orgid:00000000-0000-4000-8000-0000000000e0",
        "28:",
        "28:not-a-guid",
        "28:00000000-0000-4000-8000-0000000000E0",
        "28:00000000-0000-4000-8000-0000000000e0 ",
        "28:00000000-0000-4000-8000-0000000000e0,28:00000000-0000-4000-8000-0000000000e1",
        "28:cloud:00000000-0000-4000-8000-0000000000e0",
        "",
    ] {
        let error = record_service_mri(&locations, "echo", mri).unwrap_err();
        assert!(matches!(error, ConfigError::NotABotMri(_)), "{mri:?}: {error:?}");
    }
    assert_eq!(fs::read_to_string(&locations.config_file).unwrap(), before);

    // once recorded, another MRI isn't put in its place
    record_service_mri(&locations, "echo", BOT).unwrap();
    let recorded = fs::read_to_string(&locations.config_file).unwrap();
    let conflict = record_service_mri(&locations, "echo", "28:00000000-0000-4000-8000-0000000000e1").unwrap_err();
    assert!(
        matches!(conflict, ConfigError::ServiceMriConflict(ref name) if name == "echo"),
        "{conflict:?}"
    );
    assert_eq!(fs::read_to_string(&locations.config_file).unwrap(), recorded);
    assert_eq!(names_in(dir.path()).unwrap(), ["lab.toml"]);
}

#[test]
fn a_service_mri_another_participant_has_is_refused() {
    let text = COMMENTED.replace("mri = \"<mri:c>\"", &format!("mri = \"{BOT}\""));
    let (_dir, locations) = lab_file(&text).unwrap();
    let before = fs::read_to_string(&locations.config_file).unwrap();
    let taken = record_service_mri(&locations, "echo", BOT).unwrap_err();
    assert!(matches!(taken, ConfigError::DuplicateMri(_)), "{taken:?}");
    assert_eq!(fs::read_to_string(&locations.config_file).unwrap(), before);
}

const GROUP: &str = "19:0123456789abcdef0123456789abcdef@thread.v2";

#[test]
fn no_chats_file_is_no_chats_and_the_a_b_c_chat_is_recorded_once() {
    let dir = tempfile::tempdir().unwrap();
    let paths = LabPaths::new(dir.path().to_path_buf());
    assert_eq!(paths.chats_file(), dir.path().join("chats.toml"));
    assert_eq!(LabChats::load(&paths).unwrap(), LabChats::default());

    assert_eq!(record_abc_chat(&paths, GROUP).unwrap(), ChatRecord::Recorded);
    assert_eq!(LabChats::load(&paths).unwrap().abc.as_deref(), Some(GROUP));
    let mode = fs::metadata(paths.chats_file()).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "lab data, owner only");
    assert_eq!(names_in(dir.path()).unwrap(), ["chats.toml"], "no temporary file left");

    // the same id again changes nothing; another one isn't put in its place
    let before = fs::read_to_string(paths.chats_file()).unwrap();
    assert_eq!(record_abc_chat(&paths, GROUP).unwrap(), ChatRecord::AlreadyRecorded);
    let other = "19:fedcba9876543210fedcba9876543210@thread.v2";
    assert!(matches!(
        record_abc_chat(&paths, other).unwrap_err(),
        ChatsError::Conflict
    ));
    assert_eq!(fs::read_to_string(paths.chats_file()).unwrap(), before);
}

#[test]
fn only_a_group_chat_id_is_recorded_or_read() {
    let dir = tempfile::tempdir().unwrap();
    let paths = LabPaths::new(dir.path().to_path_buf());
    for id in [
        "19:meeting_0123456789abcdef@thread.v2",
        "19:Meeting_0123456789abcdef@thread.v2",
        "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces",
        "19:0123456789abcdef@thread.tacv2",
        "19:0123@thread.v2;messageid=1",
        "19:@thread.v2",
        "19:01 23@thread.v2",
        "48:notes",
        "",
    ] {
        let error = record_abc_chat(&paths, id).unwrap_err();
        assert!(matches!(error, ChatsError::NotAGroupChat(_)), "{id:?}: {error:?}");
        assert!(!paths.chats_file().exists(), "{id:?}");
    }
    let meeting = LabChats::parse("abc = \"19:meeting_0123@thread.v2\"\n").unwrap_err();
    assert!(matches!(meeting, ChatsError::NotAGroupChat(_)), "{meeting:?}");
    let unknown = LabChats::parse("ab = \"19:0123@thread.v2\"\n").unwrap_err();
    assert!(matches!(unknown, ChatsError::Parse(_)), "{unknown:?}");
}
