//! The flake's package in the Nix store: found by its path, the kind of install read from its marker, never
//! downloaded, and the command that updates each kind.

use std::fs;
use std::path::Path;

use super::{places, tree, version};
use crate::installation::nix_install;
use crate::{Format, Installation, NixInstall, Source};

const KINDS: [NixInstall; 4] = [
    NixInstall::Profile,
    NixInstall::NixOs,
    NixInstall::HomeManager,
    NixInstall::Other,
];

#[test]
fn each_nix_install_has_its_marker_and_takes_the_linux_archive() {
    for install in KINDS {
        let format = Format::Nix(install);
        assert_eq!(Format::from_marker(&format!("{}\n", format.marker())), Some(format));
        assert_eq!(format.nix(), Some(install));
        assert_eq!(
            format.asset(version("0.1.0.142")),
            "ngteams-0.1.0.142-linux-x86_64.tar.gz"
        );
        assert_eq!(format.system_package(), None);
        assert!(!format.stages_in_app_folder());
    }
    assert_eq!(Format::Archive.nix(), None);
    assert_eq!(Format::from_marker("nix-darwin"), None);
}

#[test]
fn each_nix_install_says_the_command_that_updates_it() {
    assert_eq!(NixInstall::Profile.command(), Some("nix profile upgrade ngteams"));
    assert_eq!(
        NixInstall::NixOs.command(),
        Some("nix flake update ngteams && sudo nixos-rebuild switch --flake .")
    );
    assert_eq!(
        NixInstall::HomeManager.command(),
        Some("nix flake update ngteams && home-manager switch --flake .")
    );
    assert_eq!(NixInstall::Other.command(), None);
    assert!(NixInstall::NixOs.says().contains("system flake"));
    assert!(NixInstall::HomeManager.says().contains("Home Manager flake"));
    assert!(NixInstall::Profile.says().ends_with(':'));
    assert!(NixInstall::Other.says().contains("Update it with Nix"));
}

/// The flake's tree in `store` as `packaging/nix/package.nix` lays it out, its marker `marker`.
fn store_tree(store: &Path, marker: &str) -> std::path::PathBuf {
    let root = store.join("abc-ngteams-unwrapped-0.1.0.142/opt/ngteams");
    tree(&root, "0.1.0.142", Format::Archive).unwrap();
    fs::write(root.join(crate::MARKER), format!("{marker}\n")).unwrap();
    root
}

#[test]
fn a_copy_in_the_nix_store_is_a_nix_install_of_the_kind_its_marker_says() {
    let home = tempfile::tempdir().unwrap();
    let store = home.path().join("nix/store");
    for (marker, install) in [
        ("nix-profile", NixInstall::Profile),
        ("nix-nixos", NixInstall::NixOs),
        ("nix-home-manager", NixInstall::HomeManager),
        // the release archive's own marker, in a package made some other way
        ("linux-archive", NixInstall::Other),
    ] {
        let root = store_tree(&store, marker);
        assert_eq!(
            nix_install(&root.join("usr/bin/ngteams"), &store),
            Some((root.clone(), install)),
            "{marker}"
        );
        fs::remove_dir_all(store.join("abc-ngteams-unwrapped-0.1.0.142")).unwrap();
    }
    // a program in the store outside a release tree: its own folder
    let bin = store.join("xyz-ngteams/bin");
    fs::create_dir_all(&bin).unwrap();
    assert_eq!(
        nix_install(&bin.join("ngteams"), &store),
        Some((bin.clone(), NixInstall::Other))
    );
    // a copy outside the store is no Nix install
    let archive = home.path().join("opt/ngteams");
    tree(&archive, "0.1.0.142", Format::Archive).unwrap();
    assert_eq!(nix_install(&archive.join("usr/bin/ngteams"), &store), None);
    assert_eq!(nix_install(Path::new("/"), Path::new("/")), None);
}

#[test]
fn a_tree_with_a_nix_marker_outside_the_store_is_a_nix_install_too() {
    let home = tempfile::tempdir().unwrap();
    let root = store_tree(&home.path().join("copied"), "nix-home-manager");
    let installation = Installation::detect(&places(root.join("usr/bin/ngteams"), home.path())).unwrap();
    assert_eq!(installation.format(), Format::Nix(NixInstall::HomeManager));
    assert_eq!(installation.root(), root);
    assert_eq!(crate::installation::NIX_STORE, "/nix/store");
}

#[test]
fn a_nix_install_never_downloads_an_update() {
    let home = tempfile::tempdir().unwrap();
    let root = store_tree(&home.path().join("nix/store"), "nix-nixos");
    let mut installation = super::archive_installation(home.path(), "0.1.0.141");
    installation.root = root;
    installation.format = Format::Nix(NixInstall::NixOs);
    let metadata = crate::check::Metadata {
        tag_name: "v0.1.0.143".into(),
        draft: false,
        prerelease: false,
        assets: ["ngteams-0.1.0.143-linux-x86_64.tar.gz", "SHA256SUMS"]
            .into_iter()
            .map(|name| crate::check::Asset {
                name: name.into(),
                browser_download_url: format!("https://example.invalid/v0.1.0.143/{name}"),
                size: 10,
            })
            .collect(),
    };
    // the check still finds the newer release, by the archive that the flake fetches
    let release = crate::check::select(
        &metadata,
        "https://example.invalid/",
        &installation,
        version("0.1.0.142"),
    )
    .unwrap()
    .unwrap();
    assert_eq!(release.asset(), "ngteams-0.1.0.143-linux-x86_64.tar.gz");
    // nothing listens on the feed: the refusal comes before any request or stage
    let error = Source::local("http://127.0.0.1:9")
        .prepare(&installation, &release, |_, _| panic!("no download"))
        .unwrap_err();
    assert_eq!(error.to_string(), crate::NIX_UPDATES);
    assert!(!home.path().join("data/updates").exists());
    let error = crate::prepare::ready(
        installation.format,
        &home.path().join("asset"),
        home.path(),
        version("0.1.0.143"),
    )
    .unwrap_err();
    assert_eq!(error.to_string(), crate::NIX_UPDATES);
    // a download that came some other way is not installed either
    let mut prepared = crate::Prepared {
        identity: installation.identity().unwrap(),
        installation,
        work: Some(tempfile::tempdir().unwrap()),
        candidate: home.path().join("candidate"),
        sha256: String::new(),
        version: version("0.1.0.143"),
    };
    assert_eq!(prepared.install().unwrap_err().to_string(), crate::NIX_UPDATES);
}
