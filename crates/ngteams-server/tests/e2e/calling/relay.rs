//! The TURN relay's credentials (`GET /trap/tokens`), read by the client's own request and reader.

use teams_proto::RelayTokens;

use crate::support::{Backend, TestServer};
use crate::trouter::{Account, server_with};

const IC3: &str = "https://ic3.teams.office.com";

async fn status(server: &TestServer, token: Option<&str>) -> u16 {
    let mut request = reqwest::Client::new().get(server.url("/trap/tokens"));
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    request.send().await.unwrap().status().as_u16()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_client_reads_a_credential_made_from_the_relays_secret() {
    let server = server_with(Backend::Sqlite, |config| {
        config.calling.turn.secret = Some("shared with coturn".to_owned());
        config.calling.turn.urls = vec!["turn:turn.own.example.test:3478?transport=udp".to_owned()];
        config.calling.turn.ttl = 600;
    })
    .await;
    let id = server.add_user("a@own.test", "Ada").await;
    let account = Account::sign_in(&server, "a@own.test", &id).await;
    let before = ngteams_server::db::now();
    let answer = account.client(&server).read(&RelayTokens).await.unwrap();
    let tokens = RelayTokens::read_answer(answer.body()).unwrap();
    assert_eq!(tokens.expires.as_secs(), 600);
    let [credential] = tokens.credentials.as_slice() else {
        panic!("one credential: {tokens:?}");
    };
    assert_eq!(credential.realm, "rtcmedia");
    let (expires, user) = credential.username.split_once(':').unwrap();
    assert_eq!(user, id);
    let expires: i64 = expires.parse().unwrap();
    assert!((before + 600..=before + 610).contains(&expires), "{expires}");
    let (_, password) = ngteams_server::calling::rest_credential("shared with coturn", &id, expires);
    assert_eq!(credential.password.expose(), password);
    let raw: serde_json::Value = serde_json::from_slice(answer.body()).unwrap();
    assert_eq!(raw["urls"][0], "turn:turn.own.example.test:3478?transport=udp");
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_relay_with_one_user_hands_that_user_out_and_one_not_configured_nothing() {
    let server = server_with(Backend::Sqlite, |config| {
        config.calling.turn.username = Some("relay-user".to_owned());
        config.calling.turn.password = Some("relay-password".to_owned());
    })
    .await;
    let id = server.add_user("a@own.test", "Ada").await;
    let account = Account::sign_in(&server, "a@own.test", &id).await;
    let answer = account.client(&server).read(&RelayTokens).await.unwrap();
    let tokens = RelayTokens::read_answer(answer.body()).unwrap();
    assert_eq!(tokens.credentials[0].username, "relay-user");
    assert_eq!(tokens.credentials[0].password.expose(), "relay-password");
    // only an account that may sign in, with its IC3 token
    let session = server.session("a@own.test").await;
    let ic3 = session.access_token(IC3).await.unwrap();
    let spaces = session.access_token(teams_auth::token::SPACES_RESOURCE).await.unwrap();
    assert_eq!(status(&server, None).await, 401);
    assert_eq!(status(&server, Some(spaces.token.expose())).await, 401);
    assert_eq!(status(&server, Some(ic3.token.expose())).await, 200);
    ngteams_server::admin::set_disabled(&server.app.db, &*server.app.fanout, "a@own.test", true)
        .await
        .unwrap();
    assert_eq!(status(&server, Some(ic3.token.expose())).await, 403);
    server.stop().await;

    let bare = server_with(Backend::Sqlite, |_| {}).await;
    bare.add_user("a@own.test", "Ada").await;
    let session = bare.session("a@own.test").await;
    let ic3 = session.access_token(IC3).await.unwrap();
    assert_eq!(status(&bare, Some(ic3.token.expose())).await, 503);
    bare.stop().await;
}
