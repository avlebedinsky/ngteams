//! The personal sign-in window's bound: the host list, the guard bound to it, the classifier's personal
//! rules, and the host gate opening a tunnel only to a host on the list (a local server standing in for Entra, through
//! `SignInHosts::for_tests`), every other connection refused and logged. Every host and value is made up.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::{Arc, mpsc};
use std::thread;

use serde_json::Value;
use teams_auth::redirect::RedirectUri;
use teams_auth::token::NATIVE_CLIENT_REDIRECT;
use teams_signin::classify::{Classification, Classifier, OwnKind};
use teams_signin::egress::HostGate;
use teams_signin::guard::{Decision, Guard};
use teams_signin::hosts::{ENTRA_HOSTS, SignInHosts};
use teams_signin_protocol::protocol::OK;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[path = "support/diagnostics.rs"]
mod diagnostics;

fn guard(hosts: SignInHosts) -> TestResult<Arc<Guard>> {
    let redirect = RedirectUri::parse(NATIVE_CLIENT_REDIRECT).map_err(|_| "redirect")?;
    let (sender, _events) = mpsc::channel();
    let guard = Guard::new(Arc::new(hosts), redirect, sender);
    assert_eq!(guard.answer("armed 1 1"), OK);
    Ok(Arc::new(guard))
}

#[test]
fn the_bound_is_entra_and_the_identity_provider_s_hosts_never_a_microsoft_service() -> TestResult {
    let hosts = SignInHosts::entra_with(&["sso.example.test".to_owned()])?;
    for host in ENTRA_HOSTS.iter().copied().chain(["sso.example.test"]) {
        assert!(hosts.admits(host, 443), "{host}");
        assert!(!hosts.admits(host, 80), "{host} on 80");
    }
    for host in [
        "teams.microsoft.com",
        "graph.microsoft.com",
        "evil.example.test",
        "127.0.0.1",
    ] {
        assert!(!hosts.admits(host, 443), "{host}");
    }
    for refused in [
        "teams.microsoft.com",
        "login.microsoftonline.com",
        "tenant.sharepoint.com",
        "x.cloud.microsoft",
        "Upper.example.test",
        "localhost",
        "-bad.example.test",
    ] {
        assert!(SignInHosts::entra_with(&[refused.to_owned()]).is_err(), "{refused}");
    }
    Ok(())
}

#[test]
fn a_refused_navigation_names_only_a_host_the_user_could_add_to_idp_hosts() -> TestResult {
    // the account names no identity provider yet: its organization's sign-in page is refused, and its host (alone,
    // never the path or the query) is the one the window's answer names
    let hosts = SignInHosts::entra_with(&[])?;
    assert_eq!(
        hosts
            .idp_candidate("https://sso.example.test/adfs/ls/?SAMLRequest=made-up&RelayState=made-up")
            .as_deref(),
        Some("sso.example.test")
    );
    assert_eq!(
        hosts.idp_candidate("https://SSO.Example.test/").as_deref(),
        Some("sso.example.test"),
        "a URL's host is lowercase"
    );
    // nothing to add: a Microsoft service, a host on the bound, an address, a page that isn't https
    for url in [
        "https://go.microsoft.com/fwlink/?LinkId=made-up",
        "https://www.office.com/",
        "https://login.microsoftonline.com:8443/x",
        "https://192.0.2.1/x",
        "http://sso.example.test/adfs/ls/",
        "https://localhost/x",
        "about:blank",
        "not a url",
    ] {
        assert_eq!(hosts.idp_candidate(url), None, "{url}");
    }
    let named = SignInHosts::entra_with(&["sso.example.test".to_owned()])?;
    assert_eq!(
        named.idp_candidate("https://sso.example.test:8443/adfs/ls/"),
        None,
        "on the bound already"
    );
    Ok(())
}

#[test]
fn the_guard_denies_every_request_off_the_bound_and_takes_the_personal_rules() -> TestResult {
    let _capture = diagnostics::capture();
    let guard = guard(SignInHosts::entra_with(&["sso.example.test".to_owned()])?)?;
    for (method, url, decision) in [
        (
            "GET",
            "https://login.microsoftonline.com/organizations/oauth2/v2.0/authorize?client_id=x",
            Decision::Allow,
        ),
        ("GET", "https://aadcdn.msftauth.net/shared/a.js", Decision::Allow),
        (
            "POST",
            "https://login.microsoftonline.com/common/SAS/BeginAuth",
            Decision::Allow,
        ),
        (
            "POST",
            "https://sso.example.test/adfs/ls/?client-request-id=x",
            Decision::Allow,
        ),
        ("GET", "https://teams.microsoft.com/v2/", Decision::Deny),
        (
            "POST",
            "https://teams.microsoft.com/api/authsvc/v1.0/authz",
            Decision::Deny,
        ),
        ("GET", "http://login.microsoftonline.com/x", Decision::Deny),
        ("GET", "http://127.0.0.1:9/x", Decision::Deny),
        (
            "POST",
            "https://login.microsoftonline.com/common/unknown",
            Decision::Deny,
        ),
    ] {
        assert_eq!(guard.decide(1, method, url), decision, "{method} {url}");
    }
    assert_eq!(diagnostics::entries().len(), 9, "every decision logged");
    let classifier = Classifier::personal(&SignInHosts::entra_with(&["sso.example.test".to_owned()])?);
    assert_eq!(
        classifier.classify("POST", "https://sso.example.test/any/path"),
        Classification::Own(OwnKind::IdentityProvider)
    );
    // the base classifier omits the additional host-bound form rules
    assert_eq!(
        Classifier::sign_in().classify("POST", "https://login.microsoftonline.com/common/SAS/EndAuth"),
        Classification::Unknown
    );
    Ok(())
}

/// A local server that answers each connection's first line with `echo: <line>` and closes.
fn echo_server() -> TestResult<u16> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let port = listener.local_addr()?.port();
    thread::spawn(move || {
        for stream in listener.incoming().map_while(Result::ok) {
            let mut line = String::new();
            let mut reader = BufReader::new(&stream);
            if reader.read_line(&mut line).is_ok() {
                let mut stream = &stream;
                let _ = stream.write_all(format!("echo: {line}").as_bytes());
            }
        }
    });
    Ok(port)
}

/// What the gate at `uri` answers `request`, read to the end.
fn ask(gate: &HostGate, request: &str) -> TestResult<String> {
    let address = gate.uri().trim_start_matches("http://").to_owned();
    let mut stream = TcpStream::connect(address)?;
    stream.write_all(request.as_bytes())?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut answer = String::new();
    stream.read_to_string(&mut answer)?;
    Ok(answer)
}

#[test]
fn the_gate_opens_a_tunnel_only_to_a_host_on_the_bound_and_logs_every_connection() -> TestResult {
    let port = echo_server()?;
    let hosts = SignInHosts::for_tests(&["127.0.0.1"], port);
    let _capture = diagnostics::capture();
    let guard = guard(hosts.clone())?;
    let gate = HostGate::start(Arc::clone(&guard), Arc::new(hosts))?;

    let answer = ask(
        &gate,
        &format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\nhello through\n"),
    )?;
    assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
    assert!(answer.ends_with("echo: hello through\n"), "{answer}");

    for refused in [
        format!("CONNECT 127.0.0.1:{} HTTP/1.1\r\n\r\n", port.wrapping_add(1)),
        "CONNECT teams.microsoft.com:443 HTTP/1.1\r\n\r\n".to_owned(),
        format!("GET http://127.0.0.1:{port}/ HTTP/1.1\r\n\r\n"),
        "nonsense\r\n\r\n".to_owned(),
    ] {
        let answer = ask(&gate, &refused)?;
        assert!(answer.starts_with("HTTP/1.1 403"), "{refused:?}: {answer}");
    }
    let entries = diagnostics::entries();
    let tunnels: Vec<&Value> = entries.iter().filter(|entry| entry["tunnel"] == true).collect();
    assert_eq!(tunnels.len(), 1, "{entries:?}");
    assert_eq!(tunnels[0]["verdict"], "allow");
    let denies = entries.iter().filter(|entry| entry["verdict"] == "deny").count();
    assert_eq!(denies, 4, "{entries:?}");
    Ok(())
}
