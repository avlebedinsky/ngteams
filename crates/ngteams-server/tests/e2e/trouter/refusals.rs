//! What the client's session never sends: a bare socket and bare requests, for the refusals and the socket's edges.

use std::time::Duration;

use bytes::Bytes;
use futures_util::{SinkExt as _, StreamExt as _};
use ngteams_server::trouter::{MAX_SOCKETS, Stack, endpoint_id, push_url};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use super::{ORIGIN, server, server_with};
use crate::support::{Backend, CLIENT_ID, PASSWORD, TestServer, backends};

pub(super) const EPID: &str = "00000000-0000-4000-8000-0000000000e1";
pub(super) const IC3: &str = "https://ic3.teams.office.com";

/// An access token of `upn` for `resource`, from the password grant.
pub(super) async fn token(server: &TestServer, upn: &str, resource: &str) -> String {
    let answer: Value = reqwest::Client::new()
        .post(server.token_endpoint())
        .form(&[
            ("client_id", CLIENT_ID),
            ("grant_type", "password"),
            ("username", upn),
            ("password", PASSWORD),
            ("scope", &format!("{resource}/.default openid profile offline_access")),
        ])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    answer["access_token"].as_str().unwrap().to_owned()
}

pub(super) fn query(epid: &str, client: &str) -> String {
    let tc = format!("%7B%22cv%22:%221%22,%22ua%22:%22{client}%22,%22hr%22:%22%22,%22v%22:%221%22%7D");
    format!("tc={tc}&timeout=40&epid={epid}&cor_id=c&con_num=1_1")
}

pub(super) struct Raw(WebSocketStream<MaybeTlsStream<TcpStream>>);

impl Raw {
    pub(super) async fn open(server: &TestServer, query: &str) -> Self {
        Self::try_open(server, query, Some(ORIGIN)).await.unwrap()
    }

    /// The socket, or the HTTP status the handshake was refused with.
    async fn try_open(server: &TestServer, query: &str, origin: Option<&str>) -> Result<Self, u16> {
        let url = format!("{}/trouter/v4/c?{query}", server.origin.replace("http://", "ws://"));
        let mut request = url.into_client_request().unwrap();
        if let Some(origin) = origin {
            request.headers_mut().insert("origin", origin.parse().unwrap());
        }
        match tokio_tungstenite::connect_async(request).await {
            Ok((socket, _)) => Ok(Self(socket)),
            Err(tokio_tungstenite::tungstenite::Error::Http(response)) => Err(response.status().as_u16()),
            Err(error) => panic!("{error}"),
        }
    }

    pub(super) async fn send(&mut self, text: &str) {
        self.0.send(Message::text(text.to_owned())).await.unwrap();
    }

    /// The next text frame; `None` once the socket closed.
    pub(super) async fn next(&mut self) -> Option<String> {
        self.next_within(Duration::from_secs(10))
            .await
            .expect("no frame in time")
    }

    pub(super) async fn next_within(&mut self, wait: Duration) -> Result<Option<String>, tokio::time::error::Elapsed> {
        tokio::time::timeout(wait, async {
            loop {
                match self.0.next().await {
                    Some(Ok(Message::Text(text))) => return Some(text.as_str().to_owned()),
                    Some(Ok(Message::Close(_)) | Err(_)) | None => return None,
                    Some(Ok(_)) => {}
                }
            }
        })
        .await
    }

    pub(super) async fn authenticate(&mut self, token: &str, params: Option<Value>) {
        assert_eq!(self.next().await.as_deref(), Some("1::"));
        let mut argument = json!({"headers": {
            "X-Ms-Test-User": "False", "Authorization": format!("Bearer {token}"), "X-MS-Migration": "True",
        }});
        if let Some(params) = params {
            argument["connectparams"] = params;
        }
        self.send(&format!(
            "5:::{}",
            json!({"name": "user.authenticate", "args": [argument]})
        ))
        .await;
    }

    /// Authenticates and reads `trouter.connected`'s argument.
    pub(super) async fn connect(&mut self, token: &str) -> Value {
        self.authenticate(token, None).await;
        let frame = self.next().await.unwrap();
        let event: Value = serde_json::from_str(frame.strip_prefix("5:1::").unwrap()).unwrap();
        assert_eq!(event["name"], "trouter.connected");
        event["args"][0].clone()
    }

    /// Whether the socket ends with this disconnect.
    async fn ends_with(&mut self, frame: &str) -> bool {
        self.next().await.as_deref() == Some(frame) && self.next().await.is_none()
    }
}

pub(super) fn registration(app: &str, registration_id: &str, path: &str, ttl: u64) -> Value {
    json!({
        "clientDescription": {"appId": app, "aesKey": "", "languageId": "en-US", "platform": "chrome",
            "templateKey": format!("{app}_2.6"), "platformUIVersion": "1/1"},
        "registrationId": registration_id, "nodeId": "",
        "transports": {"TROUTER": [{"context": "", "path": path, "ttl": ttl}]},
    })
}

pub(super) async fn register(server: &TestServer, token: Option<&str>, body: &Value) -> u16 {
    let mut request = reqwest::Client::new()
        .post(server.url("/registrar/prod/V2/registrations"))
        .header("X-MS-Migration", "True")
        .json(body);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    request.send().await.unwrap().status().as_u16()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_handshake_that_is_no_trouter_connect_is_refused() {
    let server = server(Backend::Sqlite).await;
    let good = query(EPID, "TeamsCDL");
    assert_eq!(
        Raw::try_open(&server, &good, Some("https://elsewhere.example"))
            .await
            .err(),
        Some(403)
    );
    assert_eq!(
        Raw::try_open(&server, "timeout=40", Some(ORIGIN)).await.err(),
        Some(400)
    );
    assert_eq!(
        Raw::try_open(&server, &query("NOT-A-GUID", "TeamsCDL"), Some(ORIGIN))
            .await
            .err(),
        Some(400)
    );
    let plain = reqwest::get(server.url(&format!("/trouter/v4/c?{good}")))
        .await
        .unwrap();
    assert_eq!(plain.status(), 426);
    // no `Origin` at all (not a browser) is taken
    let mut socket = Raw::try_open(&server, &good, None).await.unwrap();
    assert_eq!(socket.next().await.as_deref(), Some("1::"));
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_socket_that_does_not_authenticate_as_an_account_of_the_server_is_disconnected() {
    for backend in backends() {
        let server = server_with(backend, |config| config.trouter.auth_timeout = 1).await;
        let id = server.add_user("a@own.test", "Ada").await;
        let ic3 = token(&server, "a@own.test", IC3).await;
        let spaces = token(&server, "a@own.test", "https://api.spaces.skype.com").await;
        let q = query(EPID, "TeamsCDL");

        let mut silent = Raw::open(&server, &q).await;
        assert_eq!(silent.next().await.as_deref(), Some("1::"));
        assert!(silent.ends_with(r#"0:::{"reason":"timeout"}"#).await);

        let unauthorized = r#"0:::{"reason":"unauthorized"}"#;
        for bad in ["made-up", spaces.as_str()] {
            let mut socket = Raw::open(&server, &q).await;
            socket.authenticate(bad, None).await;
            assert!(socket.ends_with(unauthorized).await, "{}", bad.len());
        }
        for first in [
            r#"5:1+::{"name":"ping"}"#.to_owned(),
            r#"5:::{"name":"user.authenticate","args":[]}"#.to_owned(),
            format!(r#"5:::{{"name":"user.authenticate","args":[{{"headers":{{"Authorization":"{ic3}"}}}}]}}"#),
            "not a packet".to_owned(),
        ] {
            let mut socket = Raw::open(&server, &q).await;
            assert_eq!(socket.next().await.as_deref(), Some("1::"));
            socket.send(&first).await;
            assert!(socket.ends_with(unauthorized).await, "{}", first.len());
        }

        ngteams_server::admin::set_disabled(&server.app.db, "a@own.test", true)
            .await
            .unwrap();
        let mut disabled = Raw::open(&server, &q).await;
        disabled.authenticate(&ic3, None).await;
        assert!(disabled.ends_with(unauthorized).await);
        let endpoint = endpoint_id(&id, EPID, Stack::Messaging);
        let body = registration("TeamsCDLWebWorker", EPID, &push_url(ORIGIN, &endpoint), 3600);
        assert_eq!(register(&server, Some(&ic3), &body).await, 403);
        assert_eq!(server.app.trouter.open(), 0);
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_socket_answers_keep_alives_takes_answers_and_closes_when_its_token_runs_out() {
    for backend in backends() {
        let server = server_with(backend, |config| config.tokens.access_lifetime = 3).await;
        let id = server.add_user("a@own.test", "Ada").await;
        let ic3 = token(&server, "a@own.test", IC3).await;
        let mut socket = Raw::open(&server, &query(EPID, "TeamsCDL")).await;
        let info = socket.connect(&ic3).await;
        let endpoint = endpoint_id(&id, EPID, Stack::Messaging);
        assert_eq!(info["id"], endpoint);
        assert_eq!(info["surl"], push_url(ORIGIN, &endpoint));
        assert_eq!(info["reconnectUrl"], "wss://own.example.test/trouter/v4/c");
        assert_eq!(info["connectparams"]["sr"], endpoint);
        let params = &info["connectparams"];
        for name in ["issuer", "scae", "sig", "sr", "sp", "se", "st"] {
            assert!(params[name].is_string(), "{name}");
        }
        let valid: i64 = params["se"].as_str().unwrap().parse::<i64>().unwrap()
            - params["st"].as_str().unwrap().parse::<i64>().unwrap();
        assert_eq!(valid, 7 * 86_400_000);
        assert_eq!(server.app.trouter.open(), 1);

        socket.send(r#"5:7+::{"name":"ping"}"#).await;
        assert_eq!(socket.next().await.as_deref(), Some(r#"6:::7+["pong"]"#));
        socket
            .send(r#"5:8+::{"name":"user.activity","args":[{"state":"active"}]}"#)
            .await;
        assert_eq!(socket.next().await.as_deref(), Some("6:::8+[]"));
        // an unacknowledged event, an answer to a delivery and noise are taken without a word
        socket.send(r#"5:::{"name":"user.activity","args":[]}"#).await;
        socket
            .send(r#"3:::{"id":12,"status":200,"headers":{},"body":""}"#)
            .await;
        socket.send("8::").await;
        socket.send(r#"5:9+::{"name":"ping"}"#).await;
        assert_eq!(socket.next().await.as_deref(), Some(r#"6:::9+["pong"]"#));
        // the token runs out: the client is sent back for a new one
        assert!(socket.ends_with(r#"0:::{"reason":"unauthorized"}"#).await);
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_newer_socket_of_an_endpoint_replaces_the_older_one_and_a_stopping_node_closes_its_sockets() {
    for backend in backends() {
        let server = server(backend).await;
        let id = server.add_user("a@own.test", "Ada").await;
        let ic3 = token(&server, "a@own.test", IC3).await;
        let q = query(EPID, "TeamsCDL");
        // registered, so its cursor is kept and its reconnect misses nothing
        let own = push_url(ORIGIN, &endpoint_id(&id, EPID, Stack::Messaging));
        let body = registration("TeamsCDLWebWorker", EPID, &own, 3600);
        assert_eq!(register(&server, Some(&ic3), &body).await, 202);
        let mut first = Raw::open(&server, &q).await;
        let info = first.connect(&ic3).await;
        let mut other = Raw::open(&server, &query("00000000-0000-4000-8000-0000000000e2", "TeamsCDL")).await;
        other.connect(&ic3).await;
        // the same endpoint again, with the parameters the first connect handed over
        let mut second = Raw::open(&server, &q).await;
        second.authenticate(&ic3, Some(info["connectparams"].clone())).await;
        let again = second.next().await.unwrap();
        assert!(again.contains(r#""name":"trouter.connected""#) || again.contains("trouter.connected"));
        assert!(first.ends_with("0::").await);
        // the other endpoint of the account stays
        other.send(r#"5:1+::{"name":"ping"}"#).await;
        assert_eq!(other.next().await.as_deref(), Some(r#"6:::1+["pong"]"#));
        second.send(r#"5:1+::{"name":"ping"}"#).await;
        assert_eq!(second.next().await.as_deref(), Some(r#"6:::1+["pong"]"#));
        // the client leaves
        second.send("0::").await;
        assert!(second.next().await.is_none());
        // the node shuts down: the sockets left are closed
        server.app.trouter.close_all();
        assert!(other.ends_with("0::").await);
        let mut late = Raw::open(&server, &q).await;
        assert!(late.ends_with("0::").await);
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reconnecting_endpoint_whose_cursor_is_gone_is_told_of_a_loss() {
    let server = server(Backend::Sqlite).await;
    let id = server.add_user("a@own.test", "Ada").await;
    let ic3 = token(&server, "a@own.test", IC3).await;
    // an endpoint the journal never met, whose client says it was connected before
    let epid = "00000000-0000-4000-8000-0000000000e3";
    let endpoint = endpoint_id(&id, epid, Stack::Messaging);
    let mut socket = Raw::open(&server, &query(epid, "TeamsCDL")).await;
    socket.authenticate(&ic3, Some(json!({"sr": endpoint}))).await;
    assert!(socket.next().await.unwrap().contains("trouter.connected"));
    let loss = socket.next().await.unwrap();
    let event: Value = serde_json::from_str(loss.strip_prefix("5:2::").unwrap()).unwrap();
    assert_eq!(event["name"], "trouter.message_loss");
    assert_eq!(event["args"][0]["droppedIndicators"][0]["tag"], "messaging");
    socket
        .send(&format!(
            "5:1+::{}",
            json!({"name": "trouter.processed_message_loss", "args": [event["args"][0]]})
        ))
        .await;
    assert_eq!(socket.next().await.as_deref(), Some("6:::1+[]"));
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_account_holds_only_so_many_sockets_and_is_disconnected_on_request() {
    let server = server(Backend::Sqlite).await;
    server.add_user("a@own.test", "Ada").await;
    let id_b = server.add_user("b@own.test", "Bert").await;
    let ic3 = token(&server, "a@own.test", IC3).await;
    let mut held = Vec::new();
    for n in 0..MAX_SOCKETS {
        let mut socket = Raw::open(
            &server,
            &query(&format!("00000000-0000-4000-8000-{n:012x}"), "TeamsCDL"),
        )
        .await;
        socket.connect(&ic3).await;
        held.push(socket);
    }
    let mut over = Raw::open(&server, &query(EPID, "TeamsCDL")).await;
    over.authenticate(&ic3, None).await;
    assert!(over.ends_with(r#"0:::{"reason":"too_many_connections"}"#).await);
    // another account is not held back by it
    let ic3_b = token(&server, "b@own.test", IC3).await;
    let mut b = Raw::open(&server, &query(EPID, "TeamsCDL")).await;
    b.connect(&ic3_b).await;
    assert_eq!(server.app.trouter.open(), MAX_SOCKETS + 1);
    ngteams_server::trouter::disconnect(&server.app, &format!("8:orgid:{id_b}"), None)
        .await
        .unwrap();
    assert!(b.ends_with("0::").await);
    let a = format!(
        "8:orgid:{}",
        ngteams_server::db::users::by_upn(&server.app.db, "a@own.test")
            .await
            .unwrap()
            .unwrap()
            .id
    );
    ngteams_server::trouter::disconnect(&server.app, &a, Some("dup"))
        .await
        .unwrap();
    for mut socket in held {
        assert!(socket.ends_with(r#"0:::{"reason":"dup"}"#).await);
    }
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_registrar_takes_only_an_accounts_own_endpoints() {
    for backend in backends() {
        let server = server(backend).await;
        let a = server.add_user("a@own.test", "Ada").await;
        let b = server.add_user("b@own.test", "Bert").await;
        let ic3 = token(&server, "a@own.test", IC3).await;
        let spaces = token(&server, "a@own.test", "https://api.spaces.skype.com").await;
        let own = push_url(ORIGIN, &endpoint_id(&a, EPID, Stack::Messaging));
        let good = registration("TeamsCDLWebWorker", EPID, &own, 3600);
        assert_eq!(register(&server, None, &good).await, 401);
        assert_eq!(register(&server, Some(&spaces), &good).await, 401);
        assert_eq!(
            register(&server, Some(&ic3), &json!({"registrationId": EPID})).await,
            400
        );
        let not_json = reqwest::Client::new()
            .post(server.url("/registrar/prod/V2/registrations"))
            .bearer_auth(&ic3)
            .body("not json")
            .send()
            .await
            .unwrap();
        assert_eq!(not_json.status(), 400);
        // another account's endpoint, the other stack's, and a URL that isn't this server's
        let theirs = push_url(ORIGIN, &endpoint_id(&b, EPID, Stack::Messaging));
        let calling = push_url(ORIGIN, &endpoint_id(&a, EPID, Stack::Calling));
        let elsewhere = push_url("https://elsewhere.example", &endpoint_id(&a, EPID, Stack::Messaging));
        for path in [&theirs, &calling, &elsewhere] {
            let body = registration("TeamsCDLWebWorker", EPID, path, 3600);
            assert_eq!(register(&server, Some(&ic3), &body).await, 403, "{path}");
        }
        assert_eq!(
            ngteams_server::registrar::until(&server.app.db, &a, EPID, "TeamsCDLWebWorker")
                .await
                .unwrap(),
            None
        );
        // its own, for no longer than an hour; again, it is the same registration with a new end
        let long = registration("TeamsCDLWebWorker", EPID, &own, 86_400);
        assert_eq!(register(&server, Some(&ic3), &long).await, 202);
        let until = ngteams_server::registrar::until(&server.app.db, &a, EPID, "TeamsCDLWebWorker")
            .await
            .unwrap()
            .unwrap();
        assert!(until - ngteams_server::db::now() <= 3600);
        let short = registration("TeamsCDLWebWorker", EPID, &own, 60);
        assert_eq!(register(&server, Some(&ic3), &short).await, 202);
        let until = ngteams_server::registrar::until(&server.app.db, &a, EPID, "TeamsCDLWebWorker")
            .await
            .unwrap()
            .unwrap();
        assert!(until - ngteams_server::db::now() <= 60);
        // the calling stack's endpoint is its own registration
        assert_eq!(
            register(
                &server,
                Some(&ic3),
                &registration("SkypeSpacesWeb", EPID, &calling, 3600)
            )
            .await,
            202
        );
        // one that ran out is dropped by the node's housekeeping, the two that hold are left
        let brief = "00000000-0000-4000-8000-0000000000e5";
        let path = push_url(ORIGIN, &endpoint_id(&a, brief, Stack::Messaging));
        let body = registration("TeamsCDLWebWorker", brief, &path, 1);
        assert_eq!(register(&server, Some(&ic3), &body).await, 202);
        tokio::time::sleep(Duration::from_millis(2100)).await;
        ngteams_server::app::tidy(&server.app).await;
        assert_eq!(
            ngteams_server::registrar::purge_expired(&server.app.db, ngteams_server::db::now() + 7200)
                .await
                .unwrap(),
            2
        );
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_account_holds_only_so_many_registrations() {
    for backend in backends() {
        let server = server(backend).await;
        let a = server.add_user("a@own.test", "Ada").await;
        let ic3 = token(&server, "a@own.test", IC3).await;
        let body = |n: i64| {
            let epid = format!("00000000-0000-4000-8000-{n:012x}");
            let path = push_url(ORIGIN, &endpoint_id(&a, &epid, Stack::Messaging));
            registration("TeamsCDLWebWorker", &epid, &path, 3600)
        };
        let free = 4;
        for n in 0..ngteams_server::registrar::MAX_REGISTRATIONS - free {
            assert_eq!(register(&server, Some(&ic3), &body(n)).await, 202);
        }
        // requests at once take the places left and no more
        let bodies: Vec<Value> = (100..112).map(body).collect();
        let at_once = bodies.iter().map(|body| register(&server, Some(&ic3), body));
        let answers = futures_util::future::join_all(at_once).await;
        let taken = answers.iter().filter(|status| **status == 202).count();
        assert_eq!(i64::try_from(taken).unwrap(), free, "{answers:?}");
        assert!(answers.iter().all(|status| [202, 429].contains(status)), "{answers:?}");
        assert_eq!(register(&server, Some(&ic3), &body(1000)).await, 429);
        // one it holds already is renewed all the same
        assert_eq!(register(&server, Some(&ic3), &body(3)).await, 202);
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_calling_stacks_socket_registers_its_own_endpoint_and_gets_no_chat_events() {
    for backend in backends() {
        let server = server(backend).await;
        let id = server.add_user("a@own.test", "Ada").await;
        let ic3 = token(&server, "a@own.test", IC3).await;
        let mut calling = Raw::open(&server, &query(EPID, "SkypeSpaces")).await;
        let info = calling.connect(&ic3).await;
        let endpoint = endpoint_id(&id, EPID, Stack::Calling);
        assert_eq!(info["id"], endpoint);
        let mut messaging = Raw::open(&server, &query(EPID, "TeamsCDL")).await;
        let other = messaging.connect(&ic3).await;
        assert_ne!(other["id"], info["id"]);
        for (app, info) in [("SkypeSpacesWeb", &info), ("TeamsCDLWebWorker", &other)] {
            let body = registration(app, EPID, info["surl"].as_str().unwrap(), 3600);
            assert_eq!(register(&server, Some(&ic3), &body).await, 202);
        }
        let event = json!({"resourceType": "NewMessage", "resource": {
            "id": "1", "version": "1", "messagetype": "Text", "content": "x", "conversationid": "19:x@thread.v2",
        }});
        let typing = json!({"resourceType": "NewMessage", "resource": {
            "id": "2", "messagetype": "Control/Typing", "content": "", "conversationid": "19:x@thread.v2",
        }});
        let internal = json!({"resourceType": "ReadReceipt", "resource": {"id": "19:x@thread.v2"}});
        let topic = format!("push:8:orgid:{id}");
        for payload in [
            internal.to_string(),
            "not json".to_owned(),
            event.to_string(),
            typing.to_string(),
        ] {
            server.app.fanout.publish(&topic, Bytes::from(payload)).await.unwrap();
        }
        // the messaging socket gets the message and the typing, never the internal event
        let stored = messaging.next().await.unwrap();
        let delivery: Value = serde_json::from_str(stored.strip_prefix("3:::").unwrap()).unwrap();
        assert_eq!(
            delivery["url"],
            format!("/trouter/v4/f/{}/messaging", other["id"].as_str().unwrap())
        );
        assert_eq!(delivery["method"], "POST");
        let body: Value = serde_json::from_str(delivery["body"].as_str().unwrap()).unwrap();
        assert_eq!(body["resourceType"], "NewMessage");
        assert_eq!(body["resource"]["to"], "19:x@thread.v2");
        assert_eq!(body["type"], "EventMessage");
        let transient = messaging.next().await.unwrap();
        assert!(transient.contains("nostorenoloss"));
        assert!(messaging.next_within(Duration::from_millis(300)).await.is_err());
        assert!(calling.next_within(Duration::from_millis(300)).await.is_err());
        server.stop().await;
    }
}
