//! Bundle import and index on a small synthetic bundle (`tests/fixtures/mini-bundle/`, written for these tests, not
//! Microsoft code): the literal scanner, the index of URL templates, hosts, HTTP methods and routes, `bundles import`
//! from a synthetic MSIX built at test time, and `bundles search`.

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use flate2::Compression;
use flate2::write::GzEncoder;
use proto_lab::bundles::import::{self, ImportError, Imported};
use proto_lab::bundles::index::{self, Entry, INDEX_FILE, Index, IndexError, Kind};
use proto_lab::bundles::literals::literals;
use zip::write::SimpleFileOptions;

/// Helpers return errors; the tests unwrap them.
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const VERSION: &str = "1.2.3.4";
const CHAT: &str = "desktop-assets/hashed-assets/chat-5001-0a1b2c3d.js";

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mini-bundle")
}

fn fixture(relative: &str) -> TestResult<String> {
    Ok(fs::read_to_string(fixture_root().join(relative))?)
}

fn files_under(root: &Path) -> TestResult<Vec<String>> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                found.push(path.strip_prefix(root)?.to_string_lossy().into_owned());
            }
        }
    }
    found.sort();
    Ok(found)
}

/// Packs the fixture like the real MSIX: the files under `hashed-assets/` and `unhashed-assets/` gzipped with a
/// `.gz` suffix, everything else stored as is; `extra` entries are added verbatim.
fn pack_msix(dest: &Path, extra: &[(&str, &[u8])]) -> TestResult<PathBuf> {
    let root = fixture_root();
    let msix = dest.join("synthetic.msix");
    let mut writer = zip::ZipWriter::new(fs::File::create(&msix)?);
    let options = SimpleFileOptions::default();
    for relative in files_under(&root)? {
        let bytes = fs::read(root.join(&relative))?;
        if relative.starts_with("desktop-assets/") && relative.contains("hashed-assets/") {
            let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
            gzip.write_all(&bytes)?;
            writer.start_file(format!("{relative}.gz"), options)?;
            writer.write_all(&gzip.finish()?)?;
        } else {
            writer.start_file(relative, options)?;
            writer.write_all(&bytes)?;
        }
    }
    for (name, bytes) in extra {
        writer.start_file(*name, options)?;
        writer.write_all(bytes)?;
    }
    writer.finish()?;
    Ok(msix)
}

fn find<'a>(index: &'a Index, kind: Kind, value: &str) -> Option<&'a Entry> {
    index
        .entries()
        .iter()
        .find(|entry| entry.kind == kind && entry.value == value)
}

fn values(index: &Index, kind: Kind) -> Vec<&str> {
    index
        .entries()
        .iter()
        .filter(|entry| entry.kind == kind)
        .map(|entry| entry.value.as_str())
        .collect()
}

fn chat_index() -> TestResult<Index> {
    let mut builder = index::IndexBuilder::default();
    builder.add_source(CHAT, &fixture(CHAT)?);
    Ok(builder.finish())
}

#[test]
fn literals_skip_comments_and_regexes_and_keep_division() {
    let source = "/* \"c1\" */ a=/\"r\"\\//g; // \"c2\"\nb=w/2,c=\"after\",d='q\\'s',e=`t${x}u`;";
    let texts: Vec<String> = literals(source).into_iter().map(|literal| literal.text).collect();
    assert_eq!(texts, ["after", "q's", "t{}u"]);
}

#[test]
fn literals_decode_escapes_and_scan_template_expressions() {
    let source = r#"x="https:\/\/h.example.com\u002Fp\x2Fq";y=`a${f(`b${"inner"}`)}c`;"#;
    let found = literals(source);
    let texts: Vec<&str> = found.iter().map(|literal| literal.text.as_str()).collect();
    assert_eq!(texts, ["https://h.example.com/p/q", "inner", "b{}", "a{}c"]);
    let outer = found.iter().find(|literal| literal.text == "a{}c").unwrap();
    assert!(outer.template);
    assert_eq!(&source[outer.start..outer.end], "`a${f(`b${\"inner\"}`)}c`");
}

#[test]
fn literals_survive_unterminated_input() {
    for source in ["\"open", "`open ${", "a=/open", "x='line\nbreak'", "/* open"] {
        let _ = literals(source);
    }
    assert_eq!(literals("a='broken\nb=\"ok\"")[0].text, "ok");
}

#[test]
fn index_extracts_url_templates_and_hosts() {
    let index = chat_index().unwrap();
    let mut urls = values(&index, Kind::Url);
    urls.sort_unstable();
    assert_eq!(
        urls,
        [
            "https://after-division.example.com/path",
            "https://api.spaces.skype.com/api/authsvc/v1.0/authz",
            "https://chatsvcagg.teams.microsoft.com",
            "https://nested.teams.microsoft.com/inner",
            "https://{}.presence.teams.microsoft.com/v1/me/forceavailability/",
            "wss://pubsub.trouter.teams.microsoft.com:443/v4/c",
        ]
    );
    let mut hosts = values(&index, Kind::Host);
    hosts.sort_unstable();
    assert_eq!(
        hosts,
        [
            "after-division.example.com",
            "api.spaces.skype.com",
            "chatsvcagg.teams.microsoft.com",
            "nested.teams.microsoft.com",
            "notifications.skype.net",
            "pubsub.trouter.teams.microsoft.com",
            "{}.presence.teams.microsoft.com",
        ]
    );
}

#[test]
fn index_extracts_route_constants_but_not_prose_or_mime_types() {
    let index = chat_index().unwrap();
    let mut routes = values(&index, Kind::Route);
    routes.sort_unstable();
    assert_eq!(
        routes,
        [
            "/v1/threads/ALL",
            "/v1/users/ME/conversations/ALL/properties",
            "{}/api/v2/conversations/{}/messages",
            "{}/threads/{}/members",
        ]
    );
    let all: Vec<&str> = index.entries().iter().map(|entry| entry.value.as_str()).collect();
    for absent in ["/", "application/json", "window.location", "\"quoted\"", "quoted"] {
        assert!(!all.contains(&absent), "{absent} indexed");
    }
    assert!(
        !all.iter().any(|value| value.contains("comment")),
        "a comment was indexed"
    );
}

#[test]
fn index_pairs_each_http_method_with_the_nearest_url() {
    let index = chat_index().unwrap();
    let mut methods: Vec<(&str, Option<&str>)> = index
        .entries()
        .iter()
        .filter(|entry| entry.kind == Kind::Method)
        .map(|entry| (entry.value.as_str(), entry.url.as_deref()))
        .collect();
    methods.sort_unstable();
    assert_eq!(
        methods,
        [
            ("POST", Some("{}/api/v2/conversations/{}/messages")),
            (
                "PUT",
                Some("https://{}.presence.teams.microsoft.com/v1/me/forceavailability/")
            ),
            ("PUT", Some("{}/threads/{}/members")),
        ]
    );
}

#[test]
fn index_looks_into_json_and_markup_held_in_strings() {
    let source = "a=JSON.parse('{\"cfg\":{\"chat\":\"https://json.example.com/v1\",\"tags\":[\"x\"]}}');\n\
                  b=\"See <a href=\\\"https://help.example.com/doc\\\">help</a>\";";
    let mut builder = index::IndexBuilder::default();
    builder.add_source("nested.js", source);
    let index = builder.finish();
    let json = find(&index, Kind::Url, "https://json.example.com/v1").unwrap();
    assert_eq!((json.sites[0].line, json.sites[0].column), (1, 14));
    let markup = find(&index, Kind::Url, "https://help.example.com/doc").unwrap();
    assert_eq!((markup.sites[0].line, markup.sites[0].column), (2, 3));
    assert!(find(&index, Kind::Host, "help.example.com").is_some());
}

#[test]
fn index_sites_name_file_line_and_column() {
    let index = chat_index().unwrap();
    let entry = find(&index, Kind::Url, "https://chatsvcagg.teams.microsoft.com").unwrap();
    assert_eq!(entry.sites.len(), 1);
    let site = &entry.sites[0];
    assert_eq!(site.file, CHAT);
    assert_eq!(site.line, 3);
    let line = fixture(CHAT).unwrap().lines().nth(2).unwrap().to_owned();
    assert!(line[site.column - 1..].starts_with("\"https://chatsvcagg"));
}

#[test]
fn index_merges_repeated_values_across_files() {
    let mut builder = index::IndexBuilder::default();
    builder.add_source("a.js", "x=\"https://same.example.com/p\"");
    builder.add_source("b.js", "\n\ny=\"https://same.example.com/p\"");
    let index = builder.finish();
    let entry = find(&index, Kind::Url, "https://same.example.com/p").unwrap();
    let sites: Vec<(&str, usize)> = entry.sites.iter().map(|site| (site.file.as_str(), site.line)).collect();
    assert_eq!(sites, [("a.js", 1), ("b.js", 3)]);
    assert_eq!(
        index.entries().iter().filter(|entry| entry.kind == Kind::Url).count(),
        1
    );
}

#[test]
fn index_round_trips_through_jsonl() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(INDEX_FILE);
    let index = chat_index().unwrap();
    index.write(&path).unwrap();
    assert_eq!(Index::read(&path).unwrap(), index);
    let text = fs::read_to_string(&path).unwrap();
    assert_eq!(text.lines().count(), index.entries().len());
}

#[test]
fn import_unpacks_the_two_dirs_gunzipped_and_builds_the_index() {
    let dir = tempfile::tempdir().unwrap();
    let msix = pack_msix(dir.path(), &[]).unwrap();
    let bundles = dir.path().join("bundles");

    let imported = import::import(&msix, &bundles).unwrap();
    let Imported::Fresh(bundle) = imported else {
        panic!("expected a fresh import, got {imported:?}");
    };
    assert_eq!(bundle.version, VERSION);
    assert_eq!(bundle.dir, bundles.join(VERSION));
    assert_eq!(
        files_under(&bundle.dir).unwrap(),
        [
            "client-bindings/bundle.js",
            CHAT,
            "desktop-assets/serve-configs/work.json",
            "desktop-assets/unhashed-assets/index.html",
            INDEX_FILE,
        ]
    );
    assert_eq!(
        fs::read_to_string(bundle.dir.join(CHAT)).unwrap(),
        fixture(CHAT).unwrap()
    );
    assert_eq!(fs::read_dir(&bundles).unwrap().count(), 1, "staging left behind");

    let index = Index::read(&bundle.dir.join(INDEX_FILE)).unwrap();
    let delete = find(&index, Kind::Method, "DELETE").unwrap();
    // `${e}/${t}` has no letters, so it is no route; the nearest URL is the base it joins
    assert_eq!(
        delete.url.as_deref(),
        Some("https://media.calling.example.net/v1/sessions")
    );
    assert_eq!(delete.sites[0].file, "client-bindings/bundle.js");
    assert!(find(&index, Kind::Host, "presence.teams.microsoft.com").is_some());
    assert!(find(&index, Kind::Url, "https://statics.cdn.example.net/assets/").is_some());
    assert!(find(&index, Kind::Url, "https://not-unpacked.example.com/x").is_none());
}

#[test]
fn unpacked_files_are_never_executable() {
    let dir = tempfile::tempdir().unwrap();
    let msix = pack_msix(dir.path(), &[]).unwrap();
    let Imported::Fresh(bundle) = import::import(&msix, &dir.path().join("bundles")).unwrap() else {
        panic!("expected a fresh import");
    };
    for relative in files_under(&bundle.dir).unwrap() {
        let mode = fs::metadata(bundle.dir.join(&relative)).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0, "{relative} is executable");
    }
}

#[test]
fn a_second_import_finds_the_bundle_present() {
    let dir = tempfile::tempdir().unwrap();
    let msix = pack_msix(dir.path(), &[]).unwrap();
    let bundles = dir.path().join("bundles");
    assert!(matches!(import::import(&msix, &bundles).unwrap(), Imported::Fresh(_)));
    let marker = bundles.join(VERSION).join("client-bindings/bundle.js");
    fs::write(&marker, "touched").unwrap();

    let again = import::import(&msix, &bundles).unwrap();
    assert!(
        matches!(&again, Imported::Present(bundle) if bundle.version == VERSION),
        "{again:?}"
    );
    assert_eq!(fs::read_to_string(&marker).unwrap(), "touched");
}

#[test]
fn an_unfinished_import_is_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let msix = pack_msix(dir.path(), &[]).unwrap();
    let bundles = dir.path().join("bundles");
    // a version dir without an index and a stale staging dir, as a crash would leave them
    fs::create_dir_all(bundles.join(VERSION).join("stale")).unwrap();
    fs::create_dir_all(bundles.join(format!("{VERSION}.staging")).join("stale")).unwrap();

    assert!(matches!(import::import(&msix, &bundles).unwrap(), Imported::Fresh(_)));
    assert!(!bundles.join(VERSION).join("stale").exists());
    assert!(!bundles.join(format!("{VERSION}.staging")).exists());
    assert!(bundles.join(VERSION).join(INDEX_FILE).is_file());
}

/// The names left in `bundles` (none when it doesn't exist).
fn left_in(bundles: &Path) -> TestResult<Vec<String>> {
    let Ok(entries) = fs::read_dir(bundles) else {
        return Ok(Vec::new());
    };
    let mut names = Vec::new();
    for entry in entries {
        names.push(entry?.file_name().to_string_lossy().into_owned());
    }
    Ok(names)
}

#[test]
fn an_entry_escaping_the_bundle_dir_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let msix = pack_msix(dir.path(), &[("desktop-assets/../../escaped.js", b"x=1")]).unwrap();
    let bundles = dir.path().join("lab").join("bundles");

    let error = import::import(&msix, &bundles).unwrap_err();
    assert!(
        matches!(&error, ImportError::UnsafePath(name) if name.contains("escaped")),
        "{error}"
    );
    assert!(!dir.path().join("escaped.js").exists());
    assert!(!dir.path().join("lab").join("escaped.js").exists());
    assert_eq!(left_in(&bundles).unwrap(), Vec::<String>::new());
}

#[test]
fn a_broken_gzip_fails_and_leaves_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let msix = pack_msix(
        dir.path(),
        &[("desktop-assets/hashed-assets/broken.js.gz", b"not gzip")],
    )
    .unwrap();
    let bundles = dir.path().join("bundles");

    let error = import::import(&msix, &bundles).unwrap_err();
    assert!(
        matches!(&error, ImportError::Gunzip { entry, .. } if entry.ends_with("broken.js.gz")),
        "{error}"
    );
    assert_eq!(left_in(&bundles).unwrap(), Vec::<String>::new());
}

fn msix_with_manifest(dest: &Path, manifest: Option<&str>) -> TestResult<PathBuf> {
    let msix = dest.join("manifest.msix");
    let mut writer = zip::ZipWriter::new(fs::File::create(&msix)?);
    let options = SimpleFileOptions::default();
    if let Some(manifest) = manifest {
        writer.start_file("AppxManifest.xml", options)?;
        writer.write_all(manifest.as_bytes())?;
    }
    writer.start_file("client-bindings/bundle.js", options)?;
    writer.write_all(b"x=1")?;
    writer.finish()?;
    Ok(msix)
}

#[test]
fn the_version_comes_from_the_manifest_identity_and_must_be_numeric() {
    let dir = tempfile::tempdir().unwrap();
    let bundles = dir.path().join("bundles");
    let missing = msix_with_manifest(dir.path(), None).unwrap();
    assert!(matches!(
        import::import(&missing, &bundles),
        Err(ImportError::NoVersion)
    ));

    let no_identity = msix_with_manifest(dir.path(), Some("<Package version=\"1.0\"></Package>")).unwrap();
    assert!(matches!(
        import::import(&no_identity, &bundles),
        Err(ImportError::NoVersion)
    ));

    for bad in ["../1.2", "1.2.x", "", "1.2.3.4.5"] {
        let manifest = format!("<Package><Identity Name=\"T\" Version=\"{bad}\" /></Package>");
        let msix = msix_with_manifest(dir.path(), Some(&manifest)).unwrap();
        let error = import::import(&msix, &bundles).unwrap_err();
        assert!(
            matches!(&error, ImportError::BadVersion(version) if version == bad),
            "{bad}: {error}"
        );
    }
    assert_eq!(left_in(&bundles).unwrap(), Vec::<String>::new());

    let spaced = "<Package>\n<Identity\n  Name=\"T\"\n  Version=\"26.1.0\"\n/></Package>";
    let msix = msix_with_manifest(dir.path(), Some(spaced)).unwrap();
    let Imported::Fresh(bundle) = import::import(&msix, &bundles).unwrap() else {
        panic!("expected a fresh import");
    };
    assert_eq!(bundle.version, "26.1.0");
}

#[test]
fn a_missing_msix_names_its_path() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("absent.msix");
    let error = import::import(&missing, &dir.path().join("bundles")).unwrap_err();
    assert!(error.to_string().contains("absent.msix"), "{error}");
}

#[test]
fn search_matches_values_and_paired_urls_case_insensitively() {
    let dir = tempfile::tempdir().unwrap();
    let msix = pack_msix(dir.path(), &[]).unwrap();
    let bundles = dir.path().join("bundles");
    import::import(&msix, &bundles).unwrap();

    let (version, index) = index::load(&bundles, None).unwrap();
    assert_eq!(version, VERSION);
    let pattern = regex::RegexBuilder::new("MEMBERS")
        .case_insensitive(true)
        .build()
        .unwrap();
    let hits: Vec<(Kind, &str)> = index
        .search(&pattern, None)
        .map(|entry| (entry.kind, entry.value.as_str()))
        .collect();
    assert_eq!(hits, [(Kind::Route, "{}/threads/{}/members"), (Kind::Method, "PUT")]);

    let only_methods: Vec<&Entry> = index.search(&pattern, Some(Kind::Method)).collect();
    assert_eq!(only_methods.len(), 1);
    let rendered = index::render(only_methods[0]);
    assert!(rendered.starts_with("method PUT {}/threads/{}/members"), "{rendered}");
    assert!(rendered.contains(&format!("{CHAT}:6:")), "{rendered}");
}

#[test]
fn search_picks_the_newest_version_or_the_one_asked_for() {
    let dir = tempfile::tempdir().unwrap();
    let bundles = dir.path().join("bundles");
    for (version, host) in [("9.0.0", "old"), ("10.0.0", "new"), ("10.0.0.staging", "staging")] {
        let mut builder = index::IndexBuilder::default();
        builder.add_source("a.js", &format!("x=\"https://{host}.example.com/p\""));
        fs::create_dir_all(bundles.join(version)).unwrap();
        builder.finish().write(&bundles.join(version).join(INDEX_FILE)).unwrap();
    }
    // a version dir without an index is unfinished and not offered
    fs::create_dir_all(bundles.join("11.0.0")).unwrap();

    assert_eq!(index::versions(&bundles).unwrap(), ["9.0.0", "10.0.0"]);
    let (newest, index) = index::load(&bundles, None).unwrap();
    assert_eq!(newest, "10.0.0");
    assert!(find(&index, Kind::Host, "new.example.com").is_some());
    let (asked, index) = index::load(&bundles, Some("9.0.0")).unwrap();
    assert_eq!(asked, "9.0.0");
    assert!(find(&index, Kind::Host, "old.example.com").is_some());

    let error = index::load(&bundles, Some("11.0.0")).unwrap_err();
    assert!(
        matches!(&error, IndexError::UnknownVersion { version, .. } if version == "11.0.0"),
        "{error}"
    );
    assert!(error.to_string().contains("10.0.0"), "{error}");
}

#[test]
fn search_without_any_import_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let error = index::load(&dir.path().join("bundles"), None).unwrap_err();
    assert!(matches!(error, IndexError::NoBundles(_)), "{error}");
}
