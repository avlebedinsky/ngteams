//! The endpoint inventory (`docs/protocol/endpoints.md`) against the own server: every request the native client
//! sends (a row whose Native column names an entry) is served by the server, hidden by the client for an own-server
//! account (`teams_core::Capabilities`, from the GTMs the server's authz names), fetched from a public host without
//! credentials, or a known gap that names why.

use ngteams_server::config::Config;
use teams_core::Capabilities;

const INVENTORY: &str = include_str!("../../../../docs/protocol/endpoints.md");

/// What the own server does with one request the client sends.
#[derive(Debug, PartialEq, Eq)]
enum Status {
    Served,
    Hidden(String),
    PublicHost,
    NotServed(String),
}

/// One row of the inventory the native client sends: its line, its method and path, and its own-server status.
#[derive(Debug)]
struct Row {
    line: usize,
    what: String,
    status: Option<Status>,
}

/// The text in the parentheses after `marker` in `row`.
fn named(row: &str, marker: &str) -> Option<String> {
    let at = row.find(marker)? + marker.len();
    let rest = &row[at..];
    Some(rest[..rest.find(')')?].trim().to_owned())
}

fn status(row: &str) -> Option<Status> {
    let row = row.to_lowercase();
    if let Some(capability) = named(&row, "own server: hidden (") {
        return Some(Status::Hidden(capability));
    }
    if let Some(reason) = named(&row, "own server: not served (") {
        return Some(Status::NotServed(reason));
    }
    if row.contains("own server: public host") {
        return Some(Status::PublicHost);
    }
    row.contains("own server:").then_some(Status::Served)
}

/// The rows of the inventory's tables whose Native column names a native entry.
fn native_rows() -> Vec<Row> {
    INVENTORY
        .lines()
        .enumerate()
        .filter(|(_, line)| line.starts_with("| `"))
        .filter_map(|(at, line)| {
            let cells: Vec<&str> = line.trim().trim_matches('|').split('|').map(str::trim).collect();
            let native = cells.get(3)?;
            if cells.len() < 6 || native.starts_with("not used") || native.starts_with("no service") {
                return None;
            }
            Some(Row {
                line: at + 1,
                what: format!("{} {}", cells[0], cells[1]),
                status: status(line),
            })
        })
        .collect()
}

/// Whether a server of `config` leaves the client's `capability` off.
fn left_off(config: &Config, capability: &str) -> Option<bool> {
    let gtms = ngteams_server::services::region_gtms(config);
    let own = teams_proto::Cloud::own("https://own.example.test").unwrap();
    let capabilities = Capabilities::of(&own, |gtm| gtms.contains_key(gtm));
    let on = match capability {
        "workflows" => capabilities.workflows,
        "recap" => capabilities.recap,
        "captions" => capabilities.captions,
        "recording" => capabilities.recording,
        "apps" => capabilities.apps,
        "copilot" => capabilities.copilot,
        _ => return None,
    };
    Some(!on)
}

#[test]
fn every_request_the_client_sends_is_served_hidden_public_or_a_named_gap() {
    let rows = native_rows();
    assert!(rows.len() > 150, "the inventory's tables are read: {} rows", rows.len());
    let unmarked: Vec<String> = rows
        .iter()
        .filter(|row| row.status.is_none())
        .map(|row| format!("line {}: {}", row.line, row.what))
        .collect();
    assert!(
        unmarked.is_empty(),
        "rows the client sends without an own-server status (`own server: served`, `own server: hidden (<capability>)`, \
         `own server: public host`, `own server: not served (<why>)`):\n{}",
        unmarked.join("\n")
    );
    let mut off = Config::default();
    off.workflows.enabled = false;
    for row in &rows {
        match &row.status {
            Some(Status::Hidden(capability)) => assert_eq!(
                left_off(&off, capability),
                Some(true),
                "line {}: `{capability}` is no capability the client hides without its GTM",
                row.line
            ),
            Some(Status::NotServed(reason)) => {
                assert!(!reason.is_empty(), "line {}: a gap names why", row.line);
            }
            _ => {}
        }
    }
}

#[test]
fn a_default_server_names_workflows_and_leaves_every_other_optional_service_off() {
    let config = Config::default();
    assert_eq!(left_off(&config, "workflows"), Some(false));
    for capability in ["recap", "captions", "recording", "apps", "copilot"] {
        assert_eq!(left_off(&config, capability), Some(true), "{capability}");
    }
    assert_eq!(left_off(&config, "telepathy"), None);
}
