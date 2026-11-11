//! Service lifecycle diagnostics stay payload-free, including regional arrivals and departures.
use super::*;
use std::cell::RefCell;
use teams_proto::captions::{CAPTION_SERVICE_ALTERNATE_MRI, CAPTION_SERVICE_MRI};

// Each parallel test has its own records; installing this logger never writes to stdout or disk.
thread_local! {
    static MESSAGES: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

struct CaptionLog;

impl log::Log for CaptionLog {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Info && metadata.target() == "teams_call::machine::captions"
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            MESSAGES.with(|messages| messages.borrow_mut().push(record.args().to_string()));
        }
    }

    fn flush(&self) {}
}

fn active_service(mri: &str) -> Value {
    let command = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/aks-prod-zz-api.callrecorder.teams.cloud.microsoft:21606/v2/oncommand/00000000-0000-4000-8000-000000000001";
    json!({"participants": {mri: {"version": 1, "state": "active", "endpoints": {
        "synthetic": {"endpointMetadata": {"commandUrl": command,
            "processingModes": {"closedCaptions": {"state": "Active"}}}}
    }}}})
}

#[test]
fn regional_service_diagnostics_report_lifecycle_without_roster_payloads() {
    static LOGGER: CaptionLog = CaptionLog;
    log::set_logger(&LOGGER).unwrap();
    log::set_max_level(log::LevelFilter::Info);
    let now = Instant::now();
    let (mut calls, call) = connected_call(now);
    let roster = calls
        .call
        .as_ref()
        .unwrap()
        .path_of(Kind::Conversation, "rosterUpdate")
        .unwrap()
        .to_owned();
    MESSAGES.with(|messages| messages.borrow_mut().clear());
    for mri in [CAPTION_SERVICE_MRI, CAPTION_SERVICE_ALTERNATE_MRI] {
        calls.on_delivery(&delivery(&roster, &active_service(mri)), Observed::Nothing, now);
        assert_eq!(calls.caption_service(call).unwrap().mri, mri);
        let departed = json!({"participants": {mri: {"version": 2, "state": "disconnected"}}});
        calls.on_delivery(&delivery(&roster, &departed), Observed::Nothing, now);
        assert!(calls.caption_service(call).is_none());
    }
    let prefix = format!("call {}: caption service ", call.get());
    MESSAGES.with(|messages| {
        assert_eq!(
            *messages.borrow(),
            [
                format!("{prefix}in the call, captions Active"),
                format!("{prefix}not in the call"),
                format!("{prefix}in the call from another region, captions Active"),
                format!("{prefix}not in the call"),
            ]
        );
    });
    assert_eq!(calls.current(), Some((call, Some(CallState::Connected))));
}
