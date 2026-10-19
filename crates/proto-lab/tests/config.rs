//! Parsing and validating `lab.toml`: exactly the 3 lab accounts `a`, `b`, `c`, all in one tenant domain, plus the
//! approved service participants and the lab directories.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use proto_lab::config::{Alias, ConfigError, LabConfig, LabLocations};

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
