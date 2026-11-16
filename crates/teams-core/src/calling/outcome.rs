//! What a call write's last attempt comes to for the signaling machine.

use teams_call::{Outcome, Tracker};

use crate::outbox::CallTried;

/// The machine's outcome of a call write to `url` whose last attempt was `tried`. A write not sent to a link of a ring
/// whose end the tracker saw already is [`Outcome::RingOver`]: that end comes to the machine after it, on the socket,
/// and says how the ring ended. Any other write not answered is [`Outcome::Failed`].
pub(super) fn outcome(tried: CallTried, url: Option<&str>, tracker: &Tracker) -> Outcome {
    match tried {
        CallTried::Answered(answer) => Outcome::Answered {
            status: answer.status(),
            body: answer.body().to_vec(),
        },
        CallTried::NotSent(_) if url.is_some_and(|url| tracker.ring_ended(url)) => Outcome::RingOver,
        _ => Outcome::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::outcome;
    use crate::outbox::CallTried;
    use base64::Engine as _;
    use serde_json::{Value, json};
    use std::time::Duration;
    use teams_call::{Delivery, Observed, Outcome, Tracker};
    use teams_mock::{LAB_A, LAB_B, TrouterMock};
    use teams_proto::{Endpoint, Method, Mri, Request, Response, Service};

    const ENDPOINT: &str = "/v4/f/SyntheticEndpoint/";
    const AGENT: &str = "/v4/f/SyntheticEndpoint/callAgent/agent-1";

    /// The `attach` b's client sends for an invitation, its ring's end bound to its call agent's callback.
    struct Attach(reqwest::Url);

    impl Request for Attach {
        fn service(&self) -> Service {
            Service::Calling
        }
        fn method(&self) -> Method {
            Method::Post
        }
        fn endpoint(&self) -> Endpoint {
            Endpoint::new([] as [&str; 0])
        }
        fn body(&self) -> Option<Value> {
            Some(json!({"links": {
                "progress": format!("https://synthetic.trouter.invalid{AGENT}/progress/call/progress/"),
                "end": format!("https://synthetic.trouter.invalid{AGENT}/end/call/end/")
            }}))
        }
        fn call_url(&self) -> Option<&reqwest::Url> {
            Some(&self.0)
        }
    }

    fn delivery(id: i64, path: &str, body: &Value) -> Delivery {
        Delivery {
            connection: "synthetic-socket".to_owned(),
            id,
            path: path.to_owned(),
            trouter_request: Some("synthetic-request".to_owned()),
            chain_id: None,
            message_id: None,
            body: body.to_string().into_bytes(),
        }
    }

    /// b's tracker with a's invitation offered and its `attach` sent: it rings. The invitation's `attach` link.
    fn ringing() -> (Tracker, String) {
        let tracker = Tracker::new(Mri::new(LAB_B));
        let invitation = TrouterMock::call_invitation(LAB_A, LAB_B, |_| {});
        assert_eq!(
            tracker.observe_delivery(&delivery(1, ENDPOINT, &invitation)),
            Observed::Offered
        );
        let gp = base64::engine::general_purpose::STANDARD
            .decode(invitation["gp"].as_str().unwrap())
            .unwrap();
        let gp: Value = serde_json::from_slice(&gp).unwrap();
        let attach = gp["callNotification"]["links"]["attach"].as_str().unwrap().to_owned();
        tracker.before(&Attach(reqwest::Url::parse(&attach).unwrap()));
        (tracker, attach)
    }

    fn not_sent() -> CallTried {
        CallTried::NotSent("refused: the call is over".to_owned())
    }

    #[test]
    fn a_write_not_sent_for_a_ring_whose_end_came_is_ring_over() {
        let (tracker, attach) = ringing();
        assert_eq!(
            outcome(not_sent(), Some(&attach), &tracker),
            Outcome::Failed,
            "it still rings: a write not sent fails"
        );

        let end = json!({"callEnd": {"code": 487, "subCode": 0, "phrase": "Call cancelled by the caller."}});
        assert_eq!(
            tracker.observe_delivery(&delivery(2, &format!("{AGENT}/end/call/end/"), &end)),
            Observed::RingEnded
        );
        assert_eq!(outcome(not_sent(), Some(&attach), &tracker), Outcome::RingOver);

        // only a write not sent, only to that ring's links
        for (tried, url) in [
            (not_sent(), None),
            (
                not_sent(),
                Some("https://api.flightproxy.teams.microsoft.com/api/v2/ep/elsewhere/progress"),
            ),
            (
                CallTried::Denied("another account rings".to_owned()),
                Some(attach.as_str()),
            ),
            (CallTried::Uncertain("timed out".to_owned()), Some(attach.as_str())),
            (CallTried::Throttled(Duration::from_secs(1)), Some(attach.as_str())),
        ] {
            assert_eq!(outcome(tried, url, &tracker), Outcome::Failed, "{url:?}");
        }
        assert_eq!(
            outcome(
                CallTried::Answered(Response::new(404, "{}".to_owned())),
                Some(&attach),
                &tracker
            ),
            Outcome::Answered {
                status: 404,
                body: b"{}".to_vec()
            }
        );
    }

    #[test]
    fn a_write_not_sent_for_a_ring_the_client_let_go_fails() {
        let (tracker, attach) = ringing();
        tracker.let_go(&attach);
        assert_eq!(outcome(not_sent(), Some(&attach), &tracker), Outcome::Failed);
    }
}
