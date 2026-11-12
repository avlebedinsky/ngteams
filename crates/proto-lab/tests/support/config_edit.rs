//! Private configuration publication failures preserve the existing file; all identities and paths are synthetic.
use super::*;

#[test]
fn malformed_documents_or_missing_stanzas_never_gain_an_identity() {
    for text in ["invalid = [", "", "accounts = {}", "[[accounts]]\nalias = 'b'"] {
        assert!(matches!(
            with_account_mri(text, Alias::A, "8:orgid:synthetic"),
            Err(ConfigError::Edit(_))
        ));
    }
    for text in [
        "invalid = [",
        "",
        "service_participants = {}",
        "[[service_participants]]\nname = 'another'",
    ] {
        assert!(matches!(
            with_service_mri(text, "echo", "8:synthetic-echo"),
            Err(ConfigError::Edit(_))
        ));
    }
}

#[test]
fn failed_config_reads_or_publication_keep_the_destination_and_remove_the_stage() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("missing.toml");
    assert!(matches!(
        replace_unchanged(&missing, "old", "new"),
        Err(ConfigError::Read { .. })
    ));
    let locations = LabLocations {
        config_file: missing,
        data_dir: root.path().join("lab"),
        personal_data_dir: root.path().join("personal"),
    };
    assert!(matches!(
        record_service_mri(&locations, "echo", "8:synthetic-echo"),
        Err(ConfigError::Read { .. })
    ));
    let destination = root.path().join("directory.toml");
    fs::create_dir(&destination).unwrap();
    assert!(matches!(
        replace_file(&destination, "must not publish"),
        Err(ConfigError::Write { .. })
    ));
    assert!(destination.is_dir());
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    assert_eq!(fs::read_dir(destination).unwrap().count(), 0);
}
