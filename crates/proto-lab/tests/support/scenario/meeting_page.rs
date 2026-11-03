//! Shared synthetic scheduling and prejoin wire, with no browser or tenant.

use serde_json::{Value, json};

use crate::scenario::ab_chat_full::tests::{EventsOut, broadcast, page_with};
use crate::scenario::test_page::{Page, evaluated};

pub(in crate::scenario) const CHAT: &str = "19:meeting_synthetic@thread.v2";
pub(in crate::scenario) const LINK: &str = "https://teams.microsoft.com/l/meetup-join/synthetic";

pub(in crate::scenario) fn creation() -> Value {
    json!({"schedulingServiceEvent":{"groupContext":{"threadId":CHAT},"links":{"join":LINK}}})
}

fn request(events: &EventsOut, method: &str, path: &str, body: &Value, status: u16) {
    broadcast(
        events,
        "Network.requestWillBeSent",
        json!({"requestId":path,"request":{"method":method,"url":format!("https://teams.microsoft.com/api/mt/part/synthetic/v2.0/me/calendars/{path}"),"postData":body.to_string()}}),
    );
    broadcast(
        events,
        "Network.responseReceived",
        json!({"requestId":path,"response":{"status":status}}),
    );
}

pub(in crate::scenario) async fn page(body: Value, cancel_status: u16, join_status: u16) -> Page {
    page_with(false, false, move |command, events| {
        let expression = command["params"]["expression"].as_str()?;
        if expression == "-new Date().getTimezoneOffset()" {
            return Some(Ok(evaluated(json!(180))));
        }
        if expression.contains("return 'buttons: '") {
            return Some(Ok(evaluated(json!("buttons: Send | Save | Cancel meeting | Join now"))));
        }
        if expression.contains("e.scrollIntoView") {
            if expression.contains("[\"Send\",\"Save\"]") {
                request(events,"POST","default/events?synthetic=1",&body,201);
            } else if expression.contains(".pop() || document") {
                request(events,"PUT","events/synthetic/cancel",&body,cancel_status);
            } else if expression.contains("Join now") {
                broadcast(events,"Network.requestWillBeSent",json!({"requestId":"meeting-subscription","request":{"method":"POST","url":"https://synthetic.invalid/api/v2/epconv","postData":"subscription"}}));
                broadcast(events,"Network.responseReceived",json!({"requestId":"meeting-subscription","response":{"status":join_status}}));
            }
        }
        None
    }).await
}
