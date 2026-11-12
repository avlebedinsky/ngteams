use std::fs::{self, File};
use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use crate::{Pins, Progress, archive as extraction, cache, cleanup, complete, runtime_root, transfer};
use sha2::{Digest as _, Sha256};

type Reply = Box<dyn FnOnce(&str) -> Vec<u8> + Send>;

fn server(replies: Vec<Reply>) -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/runtime", listener.local_addr().unwrap());
    let thread = std::thread::spawn(move || {
        for reply in replies {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(e) => panic!("local HTTP fake did not receive the request: {e}"),
                }
            };
            socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            let response = reply(std::str::from_utf8(&request).unwrap());
            let _ = socket.write_all(&response);
        }
    });
    (url, thread)
}

fn response(body: &[u8], status: &str, headers: &str, length: usize) -> Vec<u8> {
    let mut result =
        format!("HTTP/1.1 {status}\r\nContent-Length: {length}\r\nConnection: close\r\n{headers}\r\n").into_bytes();
    result.extend(body);
    result
}

fn pins(bytes: &[u8]) -> Pins {
    Pins::from_values(
        Some("http://127.0.0.1/unused"),
        Some(&bytes.len().to_string()),
        Some(&Sha256::digest(bytes).iter().fold(String::new(), |mut hash, byte| {
            std::fmt::Write::write_fmt(&mut hash, format_args!("{byte:02x}")).unwrap();
            hash
        })),
        Some("fixture-key"),
        None,
    )
    .unwrap()
    .unwrap()
}

fn entry(builder: &mut tar::Builder<Vec<u8>>, path: &str, kind: tar::EntryType, data: &[u8], mode: u32) {
    let mut header = tar::Header::new_gnu();
    // Fill the raw name field too, allowing tests of paths tar::Header::set_path correctly refuses to produce.
    assert!(path.len() < 100);
    header.as_mut_bytes()[..path.len()].copy_from_slice(path.as_bytes());
    header.set_entry_type(kind);
    header.set_mode(mode);
    header.set_size(if kind.is_file() { data.len() as u64 } else { 0 });
    // An empty target leaves the link name field blank, as tar::Header::set_link_name refuses to.
    if kind.is_symlink() && !data.is_empty() {
        header.set_link_name(std::str::from_utf8(data).unwrap()).unwrap();
    }
    header.set_cksum();
    builder
        .append(&header, if kind.is_file() { data } else { &[] })
        .unwrap();
}

fn archive(extra: impl FnOnce(&mut tar::Builder<Vec<u8>>)) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    let manifest = format!("key=fixture-key\narch={}\nwpe_version=2.54.1\n", std::env::consts::ARCH);
    for (path, data) in [
        ("MANIFEST", manifest.as_bytes()),
        ("lib/libWPEWebKit-2.0.so.1", b"library".as_slice()),
        ("lib/wpe-webkit-2.0/WPEWebProcess", b"process"),
        ("lib/wpe-webkit-2.0/WPENetworkProcess", b"network"),
        ("libexec/gstreamer-1.0/gst-plugin-scanner", b"scanner"),
    ] {
        entry(&mut builder, path, tar::EntryType::Regular, data, 0o755);
    }
    for path in [
        "./",
        "lib/gio/modules",
        "lib/gstreamer-1.0",
        "lib/wpe-webkit-2.0/injected-bundle",
    ] {
        entry(&mut builder, path, tar::EntryType::Directory, &[], 0o755);
    }
    extra(&mut builder);
    zstd::stream::encode_all(builder.into_inner().unwrap().as_slice(), 1).unwrap()
}

fn offline(state: &Path, bytes: &[u8]) -> Result<PathBuf, crate::Error> {
    let input = state.join("input.tar.zst");
    fs::write(&input, bytes).unwrap();
    pins(bytes).install(state, &input)
}

#[test]
fn pins_are_all_or_nothing_and_override_changes_only_the_url() {
    assert!(
        Pins::from_values(None, None, None, None, Some("http://mirror"))
            .unwrap()
            .is_none()
    );
    assert!(Pins::from_values(None, None, None, Some("key"), None).is_err());
    let hash = "a".repeat(64);
    let values = |size, key, hash| {
        Pins::from_values(
            Some("https://original"),
            Some(size),
            Some(hash),
            Some(key),
            Some("http://mirror"),
        )
    };
    let pinned = values("12", "key", &hash).unwrap().unwrap();
    assert_eq!(pinned.url, "http://mirror");
    assert_eq!(
        (pinned.size, pinned.sha256.as_str(), pinned.key.as_str()),
        (12, hash.as_str(), "key")
    );
    for (size, key, hash) in [
        ("0", "key", hash.as_str()),
        ("x", "key", &hash),
        ("1", "../key", &hash),
        ("1", "key", "no"),
    ] {
        assert!(values(size, key, hash).is_err());
    }
}

#[test]
fn download_success_reports_progress_and_reuses_the_complete_runtime() {
    let bytes = archive(|b| {
        entry(b, "lib/real", tar::EntryType::Regular, b"target", 0o6755);
        entry(b, "lib/link", tar::EntryType::Symlink, b"real", 0o777);
    });
    let body = bytes.clone();
    let (url, thread) = server(vec![Box::new(move |_| response(&body, "200 OK", "", body.len()))]);
    let mut pins = pins(&bytes);
    pins.url = url;
    let state = tempfile::tempdir().unwrap();
    let mut progress = Vec::new();
    let directory = pins.download(state.path(), |p| progress.push(p)).unwrap();
    thread.join().unwrap();
    assert!(complete(&directory));
    assert!(directory.join(".in-use").is_file());
    assert_eq!(fs::read(directory.join("lib/link")).unwrap(), b"target");
    assert_eq!(
        fs::metadata(directory.join("lib/real")).unwrap().permissions().mode() & 0o7777,
        0o755
    );
    assert_eq!(progress.first().unwrap().received, 0);
    assert_eq!(
        progress.last(),
        Some(&Progress {
            received: bytes.len() as u64,
            total: bytes.len() as u64
        })
    );
    assert_eq!(pins.download(state.path(), |_| {}).unwrap(), directory); // server is gone
}

#[test]
fn interrupted_body_is_resumed_with_a_validated_range() {
    let bytes = archive(|_| {});
    let cut = bytes.len() / 2;
    let first = bytes.clone();
    let second = bytes.clone();
    let (url, thread) = server(vec![
        Box::new(move |_| response(&first[..cut], "200 OK", "", first.len())),
        Box::new(move |request| {
            assert!(request.to_ascii_lowercase().contains(&format!("range: bytes={cut}-")));
            response(
                &second[cut..],
                "206 Partial Content",
                &format!("Content-Range: bytes {cut}-{}/{}\r\n", second.len() - 1, second.len()),
                second.len() - cut,
            )
        }),
    ]);
    let mut pins = pins(&bytes);
    pins.url = url;
    let state = tempfile::tempdir().unwrap();
    assert!(pins.download(state.path(), |_| {}).is_err());
    assert_eq!(
        fs::metadata(runtime_root(state.path()).join("fixture-key.download"))
            .unwrap()
            .len(),
        cut as u64
    );
    assert!(pins.download(state.path(), |_| {}).is_ok());
    thread.join().unwrap();
}

#[test]
fn a_server_ignoring_range_restarts_instead_of_appending() {
    let bytes = archive(|_| {});
    let body = bytes.clone();
    let (url, thread) = server(vec![Box::new(move |_| response(&body, "200 OK", "", body.len()))]);
    let state = tempfile::tempdir().unwrap();
    fs::create_dir_all(runtime_root(state.path())).unwrap();
    fs::write(runtime_root(state.path()).join("fixture-key.download"), &bytes[..8]).unwrap();
    let mut pins = pins(&bytes);
    pins.url = url;
    assert!(pins.download(state.path(), |_| {}).is_ok());
    thread.join().unwrap();
}

#[test]
fn bad_hash_oversized_and_truncated_bodies_never_publish() {
    for mode in ["hash", "oversize", "truncated", "range"] {
        let bytes = archive(|_| {});
        let mut body = bytes.clone();
        match mode {
            "hash" => body[0] ^= 1,
            "oversize" => body.push(0),
            "truncated" => {
                body.pop();
            }
            _ => {}
        }
        let (url, thread) = server(vec![Box::new(move |_| {
            let status = if mode == "range" {
                "206 Partial Content"
            } else {
                "200 OK"
            };
            response(&body, status, "", body.len())
        })]);
        let mut pins = pins(&bytes);
        pins.url = url;
        let state = tempfile::tempdir().unwrap();
        assert!(pins.download(state.path(), |_| {}).is_err(), "{mode}");
        assert!(!runtime_root(state.path()).join("fixture-key").exists());
        if mode == "hash" {
            assert!(!runtime_root(state.path()).join("fixture-key.download").exists());
            let body = bytes.clone();
            let (url, retry) = server(vec![Box::new(move |_| response(&body, "200 OK", "", body.len()))]);
            pins.url = url;
            assert!(pins.download(state.path(), |_| {}).is_ok());
            retry.join().unwrap();
        }
        thread.join().unwrap();
    }
}

#[test]
fn unsafe_archive_entries_are_refused() {
    for (path, kind, data) in [
        ("../escape", tar::EntryType::Regular, b"bad".as_slice()),
        ("/absolute", tar::EntryType::Regular, b"bad"),
        ("lib/escape", tar::EntryType::Symlink, b"../../escape"),
        ("lib/absolute", tar::EntryType::Symlink, b"/outside"),
        ("lib/hard", tar::EntryType::Link, b"target"),
        ("lib/device", tar::EntryType::Char, b""),
    ] {
        let bytes = archive(|b| entry(b, path, kind, data, 0o755));
        let state = tempfile::tempdir().unwrap();
        assert!(offline(state.path(), &bytes).is_err(), "{path}");
        assert!(!runtime_root(state.path()).join("fixture-key").exists());
    }
}

#[test]
fn chained_symlink_escape_and_entries_below_symlinks_are_refused() {
    for child in [false, true] {
        let bytes = archive(|b| {
            entry(b, "lib/a", tar::EntryType::Symlink, b".", 0o777);
            if child {
                entry(b, "lib/a/file", tar::EntryType::Regular, b"bad", 0o644);
            } else {
                entry(b, "lib/b", tar::EntryType::Symlink, b"a/../../outside", 0o777);
            }
        });
        let state = tempfile::tempdir().unwrap();
        assert!(offline(state.path(), &bytes).is_err());
    }
}

#[test]
fn partial_from_a_crash_is_replaced_and_incomplete_or_wrong_manifest_is_refused() {
    let state = tempfile::tempdir().unwrap();
    let partial = runtime_root(state.path()).join("fixture-key.partial");
    fs::create_dir_all(&partial).unwrap();
    fs::write(partial.join("stale"), b"crash").unwrap();
    let directory = offline(state.path(), &archive(|_| {})).unwrap();
    assert!(!directory.join("stale").exists());
    assert!(!partial.exists());
    fs::remove_dir_all(&directory).unwrap();
    let bytes = archive(|_| {});
    let mut wrong = pins(&bytes);
    wrong.key = "wrong-key".to_owned();
    let input = state.path().join("input.tar.zst");
    fs::write(&input, &bytes).unwrap();
    assert!(wrong.install(state.path(), &input).is_err());
    for missing in ["MANIFEST", "lib/libWPEWebKit-2.0.so.1"] {
        let decoded = zstd::stream::decode_all(bytes.as_slice()).unwrap();
        let mut source = tar::Archive::new(decoded.as_slice());
        let mut filtered = tar::Builder::new(Vec::new());
        for entry in source.entries().unwrap() {
            let mut entry = entry.unwrap();
            if entry.path().unwrap() != Path::new(missing) {
                filtered.append(&entry.header().clone(), &mut entry).unwrap();
            }
        }
        let filtered = zstd::stream::encode_all(filtered.into_inner().unwrap().as_slice(), 1).unwrap();
        assert!(offline(state.path(), &filtered).is_err(), "{missing}");
    }
}

#[test]
fn concurrent_installers_download_once_and_publish_one_complete_tree() {
    let bytes = archive(|_| {});
    let body = bytes.clone();
    let (url, server) = server(vec![Box::new(move |_| response(&body, "200 OK", "", body.len()))]);
    let mut pins = pins(&bytes);
    pins.url = url;
    let state = tempfile::tempdir().unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let mut threads = Vec::new();
    for _ in 0..2 {
        let pins = pins.clone();
        let state = state.path().to_owned();
        let barrier = barrier.clone();
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            pins.download(&state, |_| {})
        }));
    }
    barrier.wait();
    let results: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap().unwrap())
        .collect();
    assert_eq!(results[0], results[1]);
    assert!(complete(&results[0]));
    server.join().unwrap();
}

#[test]
fn cleanup_skips_shared_locks_and_preserves_the_current_key() {
    let state = tempfile::tempdir().unwrap();
    let directory = offline(state.path(), &archive(|_| {})).unwrap();
    let lock = File::options()
        .read(true)
        .write(true)
        .open(directory.join(".in-use"))
        .unwrap();
    lock.lock_shared().unwrap();
    assert_eq!(cleanup(state.path(), "other-key").unwrap(), 0);
    drop(lock);
    assert_eq!(cleanup(state.path(), "fixture-key").unwrap(), 0);
    assert_eq!(cleanup(state.path(), "other-key").unwrap(), 1);
    assert!(!directory.exists());
}

#[test]
fn disk_full_reports_the_needed_space() {
    let mut full = File::options().write(true).open("/dev/full").unwrap();
    let error = transfer::copy_body(&mut b"runtime".as_slice(), &mut full, 7, 0, &mut |_| {}).unwrap_err();
    assert!(error.to_string().contains("7 bytes of free space"));
    let io = std::io::Error::from_raw_os_error(libc::ENOSPC);
    assert!(cache::space_error(io.into(), 1000).to_string().contains("1000 bytes"));
}

#[test]
fn offline_install_checks_integrity_even_if_the_key_is_already_cached() {
    let state = tempfile::tempdir().unwrap();
    let bytes = archive(|_| {});
    let directory = offline(state.path(), &bytes).unwrap();
    let input = state.path().join("tampered.tar.zst");
    let mut tampered = bytes.clone();
    tampered[0] ^= 1;
    fs::write(&input, &tampered).unwrap();
    let error = pins(&bytes).install(state.path(), &input).unwrap_err();
    assert!(error.to_string().contains("SHA-256"));
    fs::write(&input, &bytes[..bytes.len() - 1]).unwrap();
    assert!(
        pins(&bytes)
            .install(state.path(), &input)
            .unwrap_err()
            .to_string()
            .contains("size")
    );
    assert!(complete(&directory));
}

#[test]
fn publication_cleans_unused_keys_but_keeps_active_keys_and_partial_directories() {
    let state = tempfile::tempdir().unwrap();
    let root = runtime_root(state.path());
    for name in ["unused", "active", "crashed.partial", "unknown"] {
        fs::create_dir_all(root.join(name)).unwrap();
        if name != "unknown" {
            File::create_new(root.join(name).join(".in-use")).unwrap();
        }
    }
    let active = File::open(root.join("active/.in-use")).unwrap();
    active.lock_shared().unwrap();
    assert!(offline(state.path(), &archive(|_| {})).is_ok());
    assert!(!root.join("unused").exists());
    for name in ["active", "crashed.partial", "unknown", "fixture-key"] {
        assert!(root.join(name).is_dir(), "{name}");
    }
    drop(active);
    pins(&archive(|_| {})).download(state.path(), |_| {}).unwrap();
    assert!(
        !root.join("active").exists(),
        "cached reuse revisits keys that were busy at publication"
    );
}

#[test]
fn unusable_resume_ranges_reset_the_prefix_and_the_next_attempt_starts_at_zero() {
    for mode in ["offset", "total", "wildcard", "short", "416"] {
        let bytes = archive(|_| {});
        let cut = bytes.len() / 2;
        let size = bytes.len();
        let body = bytes.clone();
        let (url, thread) = server(vec![
            Box::new(move |request| {
                assert!(request.to_ascii_lowercase().contains(&format!("range: bytes={cut}-")));
                let range = match mode {
                    "offset" => format!("bytes {}-{}/{size}", cut + 1, size - 1),
                    "total" => format!("bytes {cut}-{}/{}", size - 1, size + 1),
                    "wildcard" => format!("bytes {cut}-{}/*", size - 1),
                    _ => format!("bytes {cut}-{cut}/{size}"),
                };
                let status = if mode == "416" {
                    "416 Range Not Satisfiable"
                } else {
                    "206 Partial Content"
                };
                response(b"bad", status, &format!("Content-Range: {range}\r\n"), 3)
            }),
            Box::new(move |request| {
                assert!(!request.to_ascii_lowercase().contains("range:"));
                response(&body, "200 OK", "", body.len())
            }),
        ]);
        let state = tempfile::tempdir().unwrap();
        let root = runtime_root(state.path());
        fs::create_dir_all(&root).unwrap();
        let prefix = root.join("fixture-key.download");
        fs::write(&prefix, &bytes[..cut]).unwrap();
        let mut pins = pins(&bytes);
        pins.url = url;
        assert!(pins.download(state.path(), |_| {}).is_err(), "{mode}");
        assert!(
            fs::read(&prefix).unwrap().is_empty(),
            "unusable response must not append; reset {mode}"
        );
        assert!(pins.download(state.path(), |_| {}).is_ok(), "{mode}");
        thread.join().unwrap();
    }
}

#[test]
fn offline_snapshot_is_private_and_unchanged_by_source_rewrites_or_replacement() {
    let state = tempfile::tempdir().unwrap();
    let bytes = archive(|_| {});
    let pins = pins(&bytes);
    let input = state.path().join("external.tar.zst");
    fs::write(&input, &bytes).unwrap();
    let root = runtime_root(state.path());
    fs::create_dir_all(&root).unwrap();
    let mut snapshot = cache::snapshot(&pins, &root, &input).unwrap();
    assert_eq!(snapshot.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    // Rewrite the same inode, then replace the path: neither can affect the private snapshot.
    fs::write(&input, b"changed inode").unwrap();
    fs::remove_file(&input).unwrap();
    fs::write(&input, b"replacement").unwrap();
    transfer::verify(&pins, &mut snapshot).unwrap();
    let output = state.path().join("extracted");
    fs::create_dir(&output).unwrap();
    extraction::unpack(&mut snapshot, &output, &pins.key).unwrap();
    assert!(complete(&output));
    assert_eq!(fs::read(&input).unwrap(), b"replacement");
    assert_eq!(
        fs::read_dir(&root).unwrap().count(),
        0,
        "snapshot has no writable pathname"
    );
}

#[test]
fn unpack_disk_full_reports_the_whole_uncompressed_tree() {
    let bytes = archive(|b| {
        entry(b, "large-library", tar::EntryType::Regular, &vec![0; 128 * 1024], 0o644);
        entry(b, "license", tar::EntryType::Regular, &vec![0; 4096], 0o644);
    });
    let state = tempfile::tempdir().unwrap();
    let input = state.path().join("input");
    fs::write(&input, &bytes).unwrap();
    let mut file = File::open(input).unwrap();
    let needed = extraction::unpacked_size(&mut file).unwrap();
    assert!(needed > 128 * 1024 + 4096);
    assert!(needed > bytes.len() as u64);
    let output = state.path().join("output");
    fs::create_dir(&output).unwrap();
    let failure = extraction::unpack_with(&mut file, &output, "fixture-key", |path| {
        if path.file_name().unwrap() == "license" {
            Ok(File::options().write(true).open("/dev/full")?)
        } else {
            Ok(File::create_new(path)?)
        }
    })
    .unwrap_err();
    assert!(matches!(&failure, crate::Error::Io(e) if e.raw_os_error() == Some(libc::ENOSPC)));
    assert!(
        cache::space_error(failure, needed)
            .to_string()
            .contains(&format!("{needed} bytes of free space"))
    );
}

#[test]
fn verified_download_survives_a_filesystem_failure_and_retries_without_network() {
    let bytes = archive(|_| {});
    let pins = pins(&bytes); // deliberately unreachable URL
    let state = tempfile::tempdir().unwrap();
    let root = runtime_root(state.path());
    fs::create_dir_all(&root).unwrap();
    let prefix = root.join("fixture-key.download");
    fs::write(&prefix, &bytes).unwrap();
    File::create_new(root.join(".install.lock")).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o555)).unwrap();
    let failure = pins.download(state.path(), |_| {});
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(failure, Err(crate::Error::Io(_))));
    assert_eq!(fs::read(&prefix).unwrap(), bytes);
    assert!(pins.download(state.path(), |_| {}).is_ok());
    assert!(!prefix.exists());
}

#[test]
fn content_rejected_downloads_are_discarded() {
    for bytes in [
        b"not zstd".to_vec(),
        archive(|b| entry(b, "../escape", tar::EntryType::Regular, b"bad", 0o644)),
        archive(|b| entry(b, "MANIFEST", tar::EntryType::Regular, b"duplicate", 0o644)),
        archive(|b| entry(b, "MANIFEST/child", tar::EntryType::Directory, b"", 0o755)),
        archive(|b| {
            entry(
                b,
                "lib/libWPEWebKit-2.0.so.1",
                tar::EntryType::Symlink,
                b"MANIFEST",
                0o777,
            );
        }),
        archive(|b| entry(b, ".in-use", tar::EntryType::Regular, b"reserved", 0o644)),
    ] {
        let pins = pins(&bytes);
        let state = tempfile::tempdir().unwrap();
        let root = runtime_root(state.path());
        fs::create_dir_all(&root).unwrap();
        let prefix = root.join("fixture-key.download");
        fs::write(&prefix, bytes).unwrap();
        assert!(pins.download(state.path(), |_| {}).is_err());
        assert!(!prefix.exists());
    }
}

#[test]
fn offline_install_and_cached_download_reclaim_the_same_keys_prefix() {
    let state = tempfile::tempdir().unwrap();
    let bytes = archive(|_| {});
    let root = runtime_root(state.path());
    fs::create_dir_all(&root).unwrap();
    let prefix = root.join("fixture-key.download");
    fs::write(&prefix, b"interrupted").unwrap();
    offline(state.path(), &bytes).unwrap();
    assert!(!prefix.exists());
    fs::write(&prefix, b"another interrupted download").unwrap();
    pins(&bytes).download(state.path(), |_| {}).unwrap();
    assert!(!prefix.exists());
}

#[test]
fn cleanup_removes_old_downloads_but_keeps_the_current_prefix_and_unrelated_files() {
    let state = tempfile::tempdir().unwrap();
    let root = runtime_root(state.path());
    fs::create_dir_all(&root).unwrap();
    for name in ["old.download", "fixture-key.download", "other-file"] {
        fs::write(root.join(name), b"prefix").unwrap();
    }
    // Symlinks and directories that happen to have the suffix are not download files.
    fs::create_dir(root.join("directory.download")).unwrap();
    std::os::unix::fs::symlink("other-file", root.join("link.download")).unwrap();
    assert_eq!(cleanup(state.path(), "fixture-key").unwrap(), 1);
    assert!(!root.join("old.download").exists());
    for name in [
        "fixture-key.download",
        "other-file",
        "directory.download",
        "link.download",
    ] {
        assert!(root.join(name).exists(), "{name}");
    }
}

#[test]
fn a_build_without_pins_compiles_none() {
    assert!(Pins::compiled().unwrap().is_none());
}

#[test]
fn a_data_dir_that_is_no_directory_fails_both_installs_before_any_network() {
    let state = tempfile::tempdir().unwrap();
    let file = state.path().join("data");
    fs::write(&file, "not a directory").unwrap();
    let bytes = archive(|_| {});
    let input = state.path().join("input.tar.zst");
    fs::write(&input, &bytes).unwrap();
    assert!(matches!(pins(&bytes).download(&file, |_| {}), Err(crate::Error::Io(_))));
    assert!(matches!(pins(&bytes).install(&file, &input), Err(crate::Error::Io(_))));
}

#[test]
fn cache_file_paths_occupied_by_directories_fail_without_removing_their_contents() {
    let bytes = archive(|_| {});
    for name in [".install.lock", "fixture-key.download"] {
        let state = tempfile::tempdir().unwrap();
        let root = runtime_root(state.path());
        let conflict = root.join(name);
        fs::create_dir_all(&conflict).unwrap();
        fs::write(conflict.join("keep"), b"existing data").unwrap();
        let mut pins = pins(&bytes);
        pins.url = "http://127.0.0.1:1/unused".to_owned();
        let error = pins.download(state.path(), |_| {}).unwrap_err();
        assert!(matches!(&error, crate::Error::Io(_)), "{error}");
        assert!(error.to_string().contains(conflict.to_str().unwrap()), "{error}");
        assert_eq!(fs::read(conflict.join("keep")).unwrap(), b"existing data");
        assert!(!root.join("fixture-key").exists());
        assert!(!root.join("fixture-key.partial").exists());
    }
}

#[test]
fn a_missing_offline_archive_is_an_error_and_publishes_nothing() {
    let state = tempfile::tempdir().unwrap();
    let bytes = archive(|_| {});
    assert!(matches!(
        pins(&bytes).install(state.path(), &state.path().join("absent")),
        Err(crate::Error::Io(_))
    ));
    assert!(!runtime_root(state.path()).join("fixture-key").exists());
}

#[test]
fn an_archive_that_is_not_zstd_or_claims_the_lock_is_refused() {
    let state = tempfile::tempdir().unwrap();
    assert!(matches!(
        offline(state.path(), b"not an archive"),
        Err(crate::Error::Message(_))
    ));
    let claims_lock = archive(|b| entry(b, ".in-use", tar::EntryType::Regular, b"x", 0o644));
    let state = tempfile::tempdir().unwrap();
    assert!(offline(state.path(), &claims_lock).is_err());
    assert!(!runtime_root(state.path()).join("fixture-key").exists());
}

#[test]
fn errors_read_as_their_cause() {
    let error = crate::Error::Message("no engine".into());
    assert_eq!(error.to_string(), "no engine");
}

#[test]
fn a_server_that_refuses_or_fails_is_an_error_and_keeps_nothing_published() {
    let bytes = archive(|_| {});
    let state = tempfile::tempdir().unwrap();
    let mut unreachable = pins(&bytes);
    unreachable.url = "http://127.0.0.1:1/runtime".to_owned();
    assert!(matches!(
        unreachable.download(state.path(), |_| {}),
        Err(crate::Error::Http(_))
    ));
    let (url, server_thread) = server(vec![Box::new(|_| response(b"", "500 Internal Server Error", "", 0))]);
    let mut failing = pins(&bytes);
    failing.url = url;
    assert!(matches!(
        failing.download(state.path(), |_| {}),
        Err(crate::Error::Http(_))
    ));
    server_thread.join().unwrap();
    assert!(!runtime_root(state.path()).join("fixture-key").exists());
}

#[test]
fn a_kept_prefix_longer_than_the_pin_starts_over() {
    let bytes = archive(|_| {});
    let state = tempfile::tempdir().unwrap();
    let root = runtime_root(state.path());
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("fixture-key.download"), vec![0; bytes.len() + 10]).unwrap();
    let body = bytes.clone();
    let (url, server_thread) = server(vec![Box::new(move |request| {
        assert!(!request.to_ascii_lowercase().contains("range:"), "{request}");
        response(&body, "200 OK", "", body.len())
    })]);
    let mut restart = pins(&bytes);
    restart.url = url;
    assert!(restart.download(state.path(), |_| {}).is_ok());
    server_thread.join().unwrap();
}

#[test]
fn a_complete_kept_download_is_verified_without_a_request() {
    let bytes = archive(|_| {});
    let state = tempfile::tempdir().unwrap();
    let root = runtime_root(state.path());
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("fixture-key.download"), &bytes).unwrap();
    let mut offline_url = pins(&bytes);
    offline_url.url = "http://127.0.0.1:1/never".to_owned();
    assert!(offline_url.download(state.path(), |_| {}).is_ok());
}

#[test]
fn symlinks_nested_under_symlinks_or_in_a_cycle_are_refused() {
    for links in [
        [("lib/a", b"b".as_slice()), ("lib/a/c", b"d".as_slice())],
        [("lib/a", b"b".as_slice()), ("lib/b", b"a".as_slice())],
    ] {
        let bytes = archive(|b| {
            for (path, target) in links {
                entry(b, path, tar::EntryType::Symlink, target, 0o777);
            }
        });
        let state = tempfile::tempdir().unwrap();
        assert!(
            matches!(offline(state.path(), &bytes), Err(crate::Error::Message(_))),
            "{links:?}"
        );
    }
}

#[test]
fn a_regular_file_where_a_directory_is_needed_is_a_conflict() {
    let bytes = archive(|b| {
        entry(b, "lib/plain", tar::EntryType::Regular, b"file", 0o644);
        entry(b, "lib/plain/below", tar::EntryType::Regular, b"file", 0o644);
    });
    let state = tempfile::tempdir().unwrap();
    assert!(matches!(offline(state.path(), &bytes), Err(crate::Error::Message(_))));
}

#[test]
fn installing_a_runtime_already_there_reuses_it() {
    let bytes = archive(|_| {});
    let state = tempfile::tempdir().unwrap();
    let first = offline(state.path(), &bytes).unwrap();
    assert_eq!(offline(state.path(), &bytes).unwrap(), first);
}

#[test]
fn a_stray_file_at_the_partial_path_is_replaced() {
    let bytes = archive(|_| {});
    let state = tempfile::tempdir().unwrap();
    let root = runtime_root(state.path());
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("fixture-key.partial"), b"stray").unwrap();
    assert!(offline(state.path(), &bytes).is_ok());
}

#[test]
fn a_filesystem_failure_names_the_path_and_keeps_a_full_disks_raw_error() {
    let state = tempfile::tempdir().unwrap();
    let bytes = archive(|_| {});
    let absent = state.path().join("absent.tar.zst");
    let failure = pins(&bytes).install(state.path(), &absent).unwrap_err();
    assert!(failure.to_string().contains("absent.tar.zst"), "{failure}");
    let full = cache::located(std::io::Error::from_raw_os_error(libc::ENOSPC), Path::new("/x"));
    assert!(matches!(&full, crate::Error::Io(e) if e.raw_os_error() == Some(libc::ENOSPC)));
}

#[test]
fn a_symlink_without_a_target_is_refused() {
    let bytes = archive(|b| entry(b, "lib/empty-link", tar::EntryType::Symlink, b"", 0o777));
    let state = tempfile::tempdir().unwrap();
    let failure = offline(state.path(), &bytes).unwrap_err();
    assert!(
        matches!(&failure, crate::Error::Message(message) if message == "Missing symlink target."),
        "{failure}"
    );
    assert!(!runtime_root(state.path()).join("fixture-key").exists());
}

#[test]
fn a_lock_that_cannot_be_opened_names_its_path_before_any_install_or_cleanup() {
    let state = tempfile::tempdir().unwrap();
    let root = runtime_root(state.path());
    fs::create_dir_all(root.join(".install.lock")).unwrap();
    let bytes = archive(|_| {});
    let input = state.path().join("input.tar.zst");
    fs::write(&input, &bytes).unwrap();
    let mut unreachable = pins(&bytes);
    unreachable.url = "http://127.0.0.1:1/never".to_owned();
    for failure in [
        cleanup(state.path(), "fixture-key").unwrap_err(),
        unreachable.download(state.path(), |_| {}).unwrap_err(),
        pins(&bytes).install(state.path(), &input).unwrap_err(),
    ] {
        assert!(matches!(&failure, crate::Error::Io(_)), "{failure}");
        assert!(failure.to_string().contains(".install.lock"), "{failure}");
    }
    assert!(!root.join("fixture-key").exists());
}

#[test]
fn a_download_path_that_cannot_be_opened_names_it_and_sends_no_request() {
    let bytes = archive(|_| {});
    let state = tempfile::tempdir().unwrap();
    let root = runtime_root(state.path());
    fs::create_dir_all(root.join("fixture-key.download")).unwrap();
    let mut unreachable = pins(&bytes);
    unreachable.url = "http://127.0.0.1:1/never".to_owned();
    let mut reported = Vec::new();
    let failure = unreachable
        .download(state.path(), |progress| reported.push(progress))
        .unwrap_err();
    assert!(matches!(&failure, crate::Error::Io(_)), "{failure}");
    assert!(failure.to_string().contains("fixture-key.download"), "{failure}");
    assert!(reported.is_empty());
    assert!(!root.join("fixture-key").exists());
}

#[test]
fn an_unreadable_archive_file_is_a_filesystem_error_not_a_broken_archive() {
    let bytes = archive(|_| {});
    let state = tempfile::tempdir().unwrap();
    let input = state.path().join("input");
    fs::write(&input, &bytes).unwrap();
    let mut write_only = File::options().write(true).open(&input).unwrap();
    assert!(matches!(
        extraction::unpacked_size(&mut write_only),
        Err(crate::Error::Io(e)) if e.raw_os_error() == Some(libc::EBADF)
    ));
    let output = state.path().join("output");
    fs::create_dir(&output).unwrap();
    assert!(matches!(
        extraction::unpack(&mut write_only, &output, "fixture-key"),
        Err(crate::Error::Io(e)) if e.raw_os_error() == Some(libc::EBADF)
    ));
    let mut readable = File::open(&input).unwrap();
    assert!(extraction::unpacked_size(&mut readable).unwrap() > 0);
}

#[test]
fn a_manifest_that_is_not_text_is_refused() {
    let mut builder = tar::Builder::new(Vec::new());
    entry(
        &mut builder,
        "MANIFEST",
        tar::EntryType::Regular,
        &[0xff, 0xfe, 0x00],
        0o644,
    );
    let bytes = zstd::stream::encode_all(builder.into_inner().unwrap().as_slice(), 1).unwrap();
    let state = tempfile::tempdir().unwrap();
    let failure = offline(state.path(), &bytes).unwrap_err();
    assert!(failure.to_string().contains("no valid MANIFEST"), "{failure}");
    assert!(!runtime_root(state.path()).join("fixture-key").exists());
}

#[test]
fn cleanup_passes_over_entries_whose_names_are_not_utf8() {
    use std::os::unix::ffi::OsStrExt as _;

    let state = tempfile::tempdir().unwrap();
    let root = runtime_root(state.path());
    fs::create_dir_all(&root).unwrap();
    let odd = root.join(std::ffi::OsStr::from_bytes(b"old-\xff.download"));
    fs::write(&odd, b"prefix").unwrap();
    fs::write(root.join("old.download"), b"prefix").unwrap();
    assert_eq!(cleanup(state.path(), "fixture-key").unwrap(), 1);
    assert!(odd.exists());
    assert!(!root.join("old.download").exists());
}
