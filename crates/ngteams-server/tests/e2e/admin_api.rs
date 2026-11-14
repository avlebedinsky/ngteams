//! The admin API, the first-admin setup and the self-service password change over HTTP, with the client's session
//! feeling each change.

use reqwest::Method;
use serde_json::{Value, json};
use teams_auth::authz::AuthzKind;
use teams_auth::token::{SPACES_RESOURCE, Scope, TokenError};
use teams_proto::IC3_AUDIENCE;

use crate::admin::cli;
use crate::support::{Backend, PASSWORD, TestServer, backends, redis_url};

const ROOT: &str = "root@own.test";
const BOB: &str = "bob@own.test";
const SETUP: &str = "/api/admin/v1/setup";
const USERS: &str = "/api/admin/v1/users";
const AUDIT: &str = "/api/admin/v1/audit";
const ME: &str = "/api/account/v1/me";
const CHANGE: &str = "/api/account/v1/password";

async fn send(
    server: &TestServer,
    method: Method,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> reqwest::Response {
    let mut request = reqwest::Client::new().request(method, server.url(path));
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    if let Some(body) = body {
        request = request.json(&body);
    }
    request.send().await.unwrap()
}

/// The answer's status and JSON (null without a JSON body).
async fn call(
    server: &TestServer,
    method: Method,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (u16, Value) {
    let response = send(server, method, path, token, body).await;
    let status = response.status().as_u16();
    let text = response.text().await.unwrap();
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

fn error_code(answer: &Value) -> &str {
    answer["error"]["code"].as_str().unwrap_or_default()
}

fn claims(token: &str) -> Value {
    use base64::Engine as _;
    let payload = token.split('.').nth(1).unwrap();
    serde_json::from_slice(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload)
            .unwrap(),
    )
    .unwrap()
}

/// An access token for the admin API's audience, from a password sign-in.
async fn bearer(server: &TestServer, upn: &str, password: &str) -> String {
    let (status, answer) = server.password_grant(upn, password, &Scope::spaces()).await;
    assert_eq!(status, 200, "{answer}");
    answer["access_token"].as_str().unwrap().to_owned()
}

/// A setup code from the command line.
async fn setup_code(server: &TestServer) -> String {
    let printed = cli(server, &["setup-code"], "").await.unwrap();
    printed.lines().next().unwrap().to_owned()
}

fn first_account(code: Option<&str>) -> Value {
    let mut body = json!({"upn": "Root@Own.Test", "display_name": "Root", "password": PASSWORD});
    if let Some(code) = code {
        body["code"] = code.into();
    }
    body
}

/// Creates the first admin through the setup flow; its access token.
async fn first_admin(server: &TestServer) -> String {
    let code = setup_code(server).await;
    let (status, answer) = call(server, Method::POST, SETUP, None, Some(first_account(Some(&code)))).await;
    assert_eq!(status, 201, "{answer}");
    bearer(server, ROOT, PASSWORD).await
}

/// Adds an account with [`PASSWORD`] through the admin API; its id.
async fn add(server: &TestServer, admin: &str, upn: &str, name: &str) -> String {
    let body = json!({"upn": upn, "display_name": name, "password": PASSWORD});
    let (status, user) = call(server, Method::POST, USERS, Some(admin), Some(body)).await;
    assert_eq!(status, 201, "{user}");
    user["id"].as_str().unwrap().to_owned()
}

fn user_path(id: &str) -> String {
    format!("{USERS}/{id}")
}

async fn my_id(server: &TestServer, token: &str) -> String {
    let (status, me) = call(server, Method::GET, ME, Some(token), None).await;
    assert_eq!(status, 200, "{me}");
    me["id"].as_str().unwrap().to_owned()
}

fn upns(list: &Value) -> Vec<&str> {
    list["users"]
        .as_array()
        .unwrap()
        .iter()
        .map(|user| user["upn"].as_str().unwrap())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_setup_code_creates_the_first_admin_once() {
    for backend in backends() {
        let server = TestServer::start(backend).await;
        assert_eq!(
            call(&server, Method::GET, SETUP, None, None).await,
            (200, json!({"needs_admin": true, "code_required": true})),
            "{backend:?}"
        );
        let code = setup_code(&server).await;
        for wrong in [None, Some("AAAAA-AAAAA-AAAAA-AAAAA-AAAAA")] {
            let (status, answer) = call(&server, Method::POST, SETUP, None, Some(first_account(wrong))).await;
            assert_eq!((status, error_code(&answer)), (403, "SetupCode"), "{wrong:?}");
        }
        // an account the server refuses doesn't spend the code
        let mut weak = first_account(Some(&code));
        weak["password"] = "short".into();
        let (status, answer) = call(&server, Method::POST, SETUP, None, Some(weak)).await;
        assert_eq!((status, error_code(&answer)), (400, "WeakPassword"));
        // the code as a person types it: lowercase, without the dashes
        let typed = code.replace('-', "").to_lowercase();
        let (status, root) = call(&server, Method::POST, SETUP, None, Some(first_account(Some(&typed)))).await;
        assert_eq!(status, 201, "{backend:?}: {root}");
        assert_eq!(root["upn"], ROOT);
        assert_eq!(root["display_name"], "Root");
        assert_eq!((&root["admin"], &root["enabled"]), (&json!(true), &json!(true)));
        assert_eq!(root["mri"], format!("8:orgid:{}", root["id"].as_str().unwrap()));
        assert_eq!(
            call(&server, Method::GET, SETUP, None, None).await,
            (200, json!({"needs_admin": false, "code_required": false}))
        );
        // refused once an admin exists, and no new code is issued
        let (status, answer) = call(&server, Method::POST, SETUP, None, Some(first_account(Some(&code)))).await;
        assert_eq!((status, error_code(&answer)), (409, "AdminExists"));
        assert!(cli(&server, &["setup-code"], "").await.is_err());
        // the first admin signs in as any account does, and its tokens name its role
        server.session(ROOT).await.skypetoken().await.unwrap();
        let (status, answer) = server.password_grant(ROOT, PASSWORD, &Scope::spaces()).await;
        assert_eq!(status, 200);
        let access = answer["access_token"].as_str().unwrap();
        assert_eq!(claims(access)["roles"], json!(["admin"]));
        assert_eq!(claims(answer["id_token"].as_str().unwrap())["roles"], json!(["admin"]));
        let (status, me) = call(&server, Method::GET, ME, Some(access), None).await;
        assert_eq!((status, &me["upn"], &me["admin"]), (200, &json!(ROOT), &json!(true)));
        assert_eq!(me["must_change_password"], false);
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn setup_attempts_are_rate_limited() {
    let server = TestServer::start(Backend::Sqlite).await;
    let code = setup_code(&server).await;
    for _ in 0..5 {
        let wrong = first_account(Some("BBBBB-BBBBB-BBBBB-BBBBB-BBBBB"));
        assert_eq!(call(&server, Method::POST, SETUP, None, Some(wrong)).await.0, 403);
    }
    let held = send(&server, Method::POST, SETUP, None, Some(first_account(Some(&code)))).await;
    assert_eq!(held.status(), 429);
    let wait: u64 = held.headers()["retry-after"].to_str().unwrap().parse().unwrap();
    assert!((1..=300).contains(&wait), "{wait}");
    let answer: Value = held.json().await.unwrap();
    assert_eq!(error_code(&answer), "TooManyAttempts");
    assert_eq!(
        call(&server, Method::GET, SETUP, None, None).await.1["needs_admin"],
        true
    );
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn only_an_admin_reaches_the_admin_api() {
    for backend in backends() {
        let server = TestServer::start(backend).await;
        let root = first_admin(&server).await;
        let root_id = my_id(&server, &root).await;
        let bob_id = add(&server, &root, BOB, "Bob").await;
        let bob = bearer(&server, BOB, PASSWORD).await;
        // the admin's token for another service is no admin credential
        let other = server
            .session(ROOT)
            .await
            .access_token(IC3_AUDIENCE)
            .await
            .unwrap();
        let account = json!({"upn": "eve@own.test", "display_name": "Eve", "password": PASSWORD});
        let routes = [
            (Method::GET, USERS.to_owned(), None),
            (Method::POST, USERS.to_owned(), Some(account)),
            (Method::GET, user_path(&root_id), None),
            (Method::PATCH, user_path(&bob_id), Some(json!({"admin": true}))),
            (Method::DELETE, user_path(&root_id), None),
            (
                Method::POST,
                format!("{}/password", user_path(&root_id)),
                Some(json!({"password": "taken over!"})),
            ),
            (Method::GET, AUDIT.to_owned(), None),
        ];
        for (method, path, body) in routes {
            for token in [None, Some("garbage"), Some(other.token.expose())] {
                let (status, _) = call(&server, method.clone(), &path, token, body.clone()).await;
                assert_eq!(status, 401, "{backend:?} {method} {path}");
            }
            let (status, answer) = call(&server, method.clone(), &path, Some(&bob), body).await;
            assert_eq!(
                (status, error_code(&answer)),
                (403, "NotAdmin"),
                "{backend:?} {method} {path}"
            );
        }
        // nothing of it happened
        let (status, list) = call(&server, Method::GET, USERS, Some(&root), None).await;
        assert_eq!((status, upns(&list)), (200, vec![BOB, ROOT]));
        assert_eq!(list["users"][0]["admin"], false);
        assert_eq!(server.password_grant(ROOT, PASSWORD, &Scope::spaces()).await.0, 200);
        // an account that is no admin still reads itself
        let (status, me) = call(&server, Method::GET, ME, Some(&bob), None).await;
        assert_eq!((status, &me["id"], &me["admin"]), (200, &json!(bob_id), &json!(false)));
        assert!(claims(&bob).get("roles").is_none(), "no role claim for a plain account");
        assert_eq!(call(&server, Method::GET, ME, None, None).await.0, 401);
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_admin_adds_finds_and_reads_accounts() {
    for backend in backends() {
        let server = TestServer::start(backend).await;
        let root = first_admin(&server).await;
        let ada_id = add(&server, &root, "Ada@Own.Test", "Ada Lovelace").await;
        add(&server, &root, "alan@own.test", "Alan Turing").await;
        add(&server, &root, "grace@own.test", "Grace Hopper").await;
        let linus = json!({"upn": "linus@own.test", "display_name": " ", "password": PASSWORD, "admin": true});
        let (status, linus) = call(&server, Method::POST, USERS, Some(&root), Some(linus)).await;
        assert_eq!(status, 201, "{linus}");
        assert_eq!(
            (&linus["display_name"], &linus["admin"]),
            (&json!("linus@own.test"), &json!(true))
        );
        server.session("linus@own.test").await.skypetoken().await.unwrap();

        for (upn, password, status, code) in [
            ("ada@own.test", PASSWORD, 409, "UpnTaken"),
            ("not-an-upn", PASSWORD, 400, "InvalidUpn"),
            ("eve@own.test", "short", 400, "WeakPassword"),
        ] {
            let body = json!({"upn": upn, "display_name": "X", "password": password});
            let (got, answer) = call(&server, Method::POST, USERS, Some(&root), Some(body)).await;
            assert_eq!((got, error_code(&answer)), (status, code), "{backend:?} {upn}");
        }

        let (status, all) = call(&server, Method::GET, USERS, Some(&root), None).await;
        assert_eq!(status, 200);
        assert_eq!(
            upns(&all),
            ["ada@own.test", "alan@own.test", "grace@own.test", "linus@own.test", ROOT]
        );
        assert_eq!(all["next"], Value::Null);
        let ada = &all["users"][0];
        assert_eq!(
            ada,
            &json!({
                "id": ada_id,
                "mri": format!("8:orgid:{ada_id}"),
                "upn": "ada@own.test",
                "display_name": "Ada Lovelace",
                "enabled": true,
                "admin": false,
                "must_change_password": false,
                "created_at": ada["created_at"],
                "updated_at": ada["updated_at"],
            })
        );
        assert!(ada["created_at"].as_i64().unwrap() > 0);

        // pages of two, by UPN
        let (_, first) = call(&server, Method::GET, &format!("{USERS}?limit=2"), Some(&root), None).await;
        assert_eq!(upns(&first), ["ada@own.test", "alan@own.test"]);
        assert_eq!(first["next"], "alan@own.test");
        let page = format!("{USERS}?limit=2&after=alan%40own.test");
        let (_, second) = call(&server, Method::GET, &page, Some(&root), None).await;
        assert_eq!(upns(&second), ["grace@own.test", "linus@own.test"]);
        let page = format!("{USERS}?limit=2&after=linus%40own.test");
        let (_, last) = call(&server, Method::GET, &page, Some(&root), None).await;
        assert_eq!((upns(&last), &last["next"]), (vec![ROOT], &Value::Null));

        // search in names and UPNs, any case; a wildcard is a plain character
        for (query, found) in [
            ("LOVE", vec!["ada@own.test"]),
            ("gra", vec!["grace@own.test"]),
            ("own.test&limit=3", vec!["ada@own.test", "alan@own.test", "grace@own.test"]),
            ("a%25", vec![]),
            ("_", vec![]),
        ] {
            let (status, list) = call(&server, Method::GET, &format!("{USERS}?q={query}"), Some(&root), None).await;
            assert_eq!((status, upns(&list)), (200, found), "{backend:?} {query}");
        }

        let (status, one) = call(&server, Method::GET, &user_path(&ada_id), Some(&root), None).await;
        assert_eq!((status, &one), (200, ada));
        let (status, answer) = call(&server, Method::GET, &user_path("no-such-id"), Some(&root), None).await;
        assert_eq!((status, error_code(&answer)), (404, "NotFound"));
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_admin_changes_an_account() {
    for backend in backends() {
        let server = TestServer::start(backend).await;
        let root = first_admin(&server).await;
        let bob_id = add(&server, &root, BOB, "Bob").await;
        add(&server, &root, "ada@own.test", "Ada").await;
        let session = server.session(BOB).await;
        let path = user_path(&bob_id);

        let renamed = json!({"display_name": " Robert ", "upn": "Robert@Own.Test"});
        let (status, bob) = call(&server, Method::PATCH, &path, Some(&root), Some(renamed)).await;
        assert_eq!(status, 200, "{backend:?}: {bob}");
        assert_eq!(
            (&bob["upn"], &bob["display_name"], &bob["id"]),
            (&json!("robert@own.test"), &json!("Robert"), &json!(bob_id))
        );
        assert_eq!(server.password_grant(BOB, PASSWORD, &Scope::spaces()).await.0, 400);
        let token = bearer(&server, "robert@own.test", PASSWORD).await;
        assert_eq!(claims(&token)["name"], "Robert");
        // the same account: its session goes on
        session.access_token(IC3_AUDIENCE).await.unwrap();

        let (status, bob) = call(
            &server,
            Method::PATCH,
            &path,
            Some(&root),
            Some(json!({"enabled": false})),
        )
        .await;
        assert_eq!((status, &bob["enabled"]), (200, &json!(false)));
        let error = session.access_token(SPACES_RESOURCE).await.unwrap_err();
        assert!(matches!(error, TokenError::InvalidGrant(_)), "{backend:?}: {error:?}");
        let (status, answer) = server
            .password_grant("robert@own.test", PASSWORD, &Scope::spaces())
            .await;
        assert_eq!((status, answer["error_codes"][0].as_u64()), (400, Some(50057)));
        let (status, _) = call(
            &server,
            Method::PATCH,
            &path,
            Some(&root),
            Some(json!({"enabled": true})),
        )
        .await;
        assert_eq!(status, 200);
        bearer(&server, "robert@own.test", PASSWORD).await;

        for (body, status, code) in [
            (json!({"upn": "ada@own.test"}), 409, "UpnTaken"),
            (json!({"upn": "nope"}), 400, "InvalidUpn"),
            (json!({"display_name": "x".repeat(300)}), 400, "InvalidName"),
        ] {
            let (got, answer) = call(&server, Method::PATCH, &path, Some(&root), Some(body)).await;
            assert_eq!((got, error_code(&answer)), (status, code), "{backend:?}");
        }
        let (status, unchanged) = call(&server, Method::PATCH, &path, Some(&root), Some(json!({}))).await;
        assert_eq!(
            (status, &unchanged["upn"], &unchanged["display_name"]),
            (200, &json!("robert@own.test"), &json!("Robert"))
        );
        let nobody = user_path("no-such-id");
        let (status, answer) = call(&server, Method::PATCH, &nobody, Some(&root), Some(json!({}))).await;
        assert_eq!((status, error_code(&answer)), (404, "NotFound"));
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn admins_are_assigned_and_removed_but_never_the_last_one() {
    for backend in backends() {
        let server = TestServer::start(backend).await;
        let root = first_admin(&server).await;
        let root_path = user_path(&my_id(&server, &root).await);
        let bob_path = user_path(&add(&server, &root, BOB, "Bob").await);
        let bob = bearer(&server, BOB, PASSWORD).await;

        // the last admin can't demote, disable or delete itself
        for body in [json!({"admin": false}), json!({"enabled": false})] {
            let (status, answer) = call(&server, Method::PATCH, &root_path, Some(&root), Some(body)).await;
            assert_eq!((status, error_code(&answer)), (409, "LastAdmin"), "{backend:?}");
        }
        let (status, answer) = call(&server, Method::DELETE, &root_path, Some(&root), None).await;
        assert_eq!((status, error_code(&answer)), (409, "LastAdmin"), "{backend:?}");
        let (_, still) = call(&server, Method::GET, &root_path, Some(&root), None).await;
        assert_eq!((&still["admin"], &still["enabled"]), (&json!(true), &json!(true)));

        // a promoted account is an admin at once, with the token it holds
        let promote = Some(json!({"admin": true}));
        let (status, promoted) = call(&server, Method::PATCH, &bob_path, Some(&root), promote.clone()).await;
        assert_eq!((status, &promoted["admin"]), (200, &json!(true)));
        assert_eq!(call(&server, Method::GET, USERS, Some(&bob), None).await.0, 200);
        assert_eq!(
            claims(&bearer(&server, BOB, PASSWORD).await)["roles"],
            json!(["admin"])
        );

        // with two admins one may go; a demoted one is refused at once
        let demote = Some(json!({"admin": false}));
        let (status, demoted) = call(&server, Method::PATCH, &root_path, Some(&bob), demote).await;
        assert_eq!((status, &demoted["admin"]), (200, &json!(false)));
        assert_eq!(call(&server, Method::GET, USERS, Some(&root), None).await.0, 403);
        let (status, answer) = call(&server, Method::DELETE, &bob_path, Some(&bob), None).await;
        assert_eq!((status, error_code(&answer)), (409, "LastAdmin"));

        // an admin that isn't the last may delete itself
        assert_eq!(
            call(&server, Method::PATCH, &root_path, Some(&bob), promote).await.0,
            200
        );
        assert_eq!(call(&server, Method::DELETE, &bob_path, Some(&bob), None).await.0, 204);
        assert_eq!(call(&server, Method::GET, USERS, Some(&bob), None).await.0, 403);
        let (status, list) = call(&server, Method::GET, USERS, Some(&root), None).await;
        assert_eq!((status, upns(&list)), (200, vec![ROOT]));
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn deleting_an_account_signs_it_out_and_keeps_its_identity() {
    for backend in backends() {
        let server = TestServer::start(backend).await;
        let root = first_admin(&server).await;
        let bob_id = add(&server, &root, BOB, "Bob").await;
        let session = server.session(BOB).await;
        let live = session.access_token(SPACES_RESOURCE).await.unwrap();
        let path = user_path(&bob_id);

        assert_eq!(call(&server, Method::DELETE, &path, Some(&root), None).await.0, 204);
        let (status, answer) = call(&server, Method::DELETE, &path, Some(&root), None).await;
        assert_eq!((status, error_code(&answer)), (404, "NotFound"), "{backend:?}");

        // the client's session can't refresh, the password is gone, and the token it still holds opens nothing
        let error = session.access_token(IC3_AUDIENCE).await.unwrap_err();
        assert!(matches!(error, TokenError::InvalidGrant(_)), "{backend:?}: {error:?}");
        assert_eq!(server.password_grant(BOB, PASSWORD, &Scope::spaces()).await.0, 400);
        assert!(
            server
                .authz_client()
                .exchange(&live.token, AuthzKind::TokenRefresh)
                .await
                .is_err()
        );
        assert_eq!(
            call(&server, Method::GET, ME, Some(live.token.expose()), None).await.0,
            403
        );
        assert_eq!(call(&server, Method::GET, &path, Some(&root), None).await.0, 404);
        let (_, list) = call(&server, Method::GET, USERS, Some(&root), None).await;
        assert_eq!(upns(&list), [ROOT]);
        for body in [json!({"display_name": "Back"}), json!({"enabled": true})] {
            assert_eq!(
                call(&server, Method::PATCH, &path, Some(&root), Some(body)).await.0,
                404
            );
        }

        // what it authored keeps an author: the id stays, as "Deleted user"
        let kept = ngteams_server::db::users::by_id(&server.app.db, &bob_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(kept.display_name, "Deleted user");
        assert!(kept.disabled && kept.upn != BOB);

        // the name is free for a new account, which is another identity
        let again = add(&server, &root, BOB, "Bob II").await;
        assert_ne!(again, bob_id);
        server.session(BOB).await.skypetoken().await.unwrap();
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reset_password_replaces_the_old_one_and_can_force_a_change() {
    for backend in backends() {
        let server = TestServer::start(backend).await;
        let root = first_admin(&server).await;
        let bob_id = add(&server, &root, BOB, "Bob").await;
        let session = server.session(BOB).await;
        let path = format!("{}/password", user_path(&bob_id));
        let grant = async |password: &str| {
            let (status, answer) = server.password_grant(BOB, password, &Scope::spaces()).await;
            (status, answer["error_codes"][0].as_u64(), answer)
        };

        let (status, answer) = call(
            &server,
            Method::POST,
            &path,
            Some(&root),
            Some(json!({"password": "short"})),
        )
        .await;
        assert_eq!((status, error_code(&answer)), (400, "WeakPassword"));
        let reset = json!({"password": "a second password"});
        assert_eq!(
            call(&server, Method::POST, &path, Some(&root), Some(reset)).await.0,
            204
        );
        let error = session.access_token(IC3_AUDIENCE).await.unwrap_err();
        assert!(matches!(error, TokenError::InvalidGrant(_)), "{backend:?}: {error:?}");
        assert_eq!(grant(PASSWORD).await.1, Some(50126));
        assert_eq!(grant("a second password").await.0, 200);

        // a reset that must be changed at the next sign-in: the password is right, but gets no token
        let reset = json!({"password": "a temporary one", "must_change": true});
        assert_eq!(
            call(&server, Method::POST, &path, Some(&root), Some(reset)).await.0,
            204
        );
        let (_, bob) = call(&server, Method::GET, &user_path(&bob_id), Some(&root), None).await;
        assert_eq!(bob["must_change_password"], true);
        let (status, code, answer) = grant("a temporary one").await;
        assert_eq!((status, code), (400, Some(50055)), "{backend:?}: {answer}");
        assert_eq!(
            (&answer["error"], &answer["suberror"]),
            (&json!("invalid_grant"), &json!("user_password_expired"))
        );
        assert_eq!(grant("a second password").await.1, Some(50126));

        let change = |current: &str, new: &str| {
            let body = json!({"upn": "Bob@Own.Test", "current_password": current, "new_password": new});
            call(&server, Method::POST, CHANGE, None, Some(body))
        };
        for (current, new, status, code) in [
            ("a wrong password", "a third password", 403, "InvalidCredentials"),
            ("a temporary one", "a temporary one", 400, "SamePassword"),
            ("a temporary one", "short", 400, "WeakPassword"),
        ] {
            let (got, answer) = change(current, new).await;
            assert_eq!((got, error_code(&answer)), (status, code), "{backend:?} {new}");
        }
        assert_eq!(change("a temporary one", "a third password").await.0, 204);
        assert_eq!(grant("a temporary one").await.1, Some(50126));
        assert_eq!(grant("a third password").await.0, 200);
        let (_, bob) = call(&server, Method::GET, &user_path(&bob_id), Some(&root), None).await;
        assert_eq!(bob["must_change_password"], false);

        let nobody = format!("{}/password", user_path("no-such-id"));
        let reset = json!({"password": "a second password"});
        let (status, answer) = call(&server, Method::POST, &nobody, Some(&root), Some(reset)).await;
        assert_eq!((status, error_code(&answer)), (404, "NotFound"));
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn any_account_changes_its_own_password() {
    for backend in backends() {
        let server = TestServer::start_with(backend, |config| config.login.max_failures = 4).await;
        let upn = format!("ada-{}@own.test", uuid::Uuid::new_v4().simple());
        server.add_user(&upn, "Ada").await;
        server.add_user("off@own.test", "Off").await;
        ngteams_server::admin::set_disabled(&server.app.db, "off@own.test", true)
            .await
            .unwrap();
        let session = server.session(&upn).await;
        let change = |upn: &str, current: &str, new: &str| {
            let body = json!({"upn": upn, "current_password": current, "new_password": new});
            send(&server, Method::POST, CHANGE, None, Some(body))
        };

        let changed = change(&upn.to_uppercase(), PASSWORD, "ada's new password").await;
        assert_eq!(changed.status(), 204, "{backend:?}");
        // signed out everywhere; the new password signs in
        let error = session.access_token(IC3_AUDIENCE).await.unwrap_err();
        assert!(matches!(error, TokenError::InvalidGrant(_)), "{backend:?}: {error:?}");
        assert_eq!(server.password_grant(&upn, PASSWORD, &Scope::spaces()).await.0, 400);
        bearer(&server, &upn, "ada's new password").await;

        // an unknown or disabled account answers as a wrong password does
        for account in ["nobody@own.test", "off@own.test"] {
            let refused = change(account, PASSWORD, "another password").await;
            assert_eq!(refused.status(), 403, "{account}");
            let answer: Value = refused.json().await.unwrap();
            assert_eq!(error_code(&answer), "InvalidCredentials");
        }

        // guesses count as sign-in attempts do
        for _ in 0..4 {
            assert_eq!(change(&upn, "a wrong guess", "another password").await.status(), 403);
        }
        let held = change(&upn, "ada's new password", "another password").await;
        assert_eq!(held.status(), 429, "{backend:?}");
        assert!(held.headers()["retry-after"].to_str().unwrap().parse::<u64>().unwrap() > 0);
        assert_eq!(
            server
                .password_grant(&upn, "ada's new password", &Scope::spaces())
                .await
                .0,
            429
        );
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn every_admin_action_is_audited() {
    for backend in backends() {
        let server = TestServer::start(backend).await;
        let root = first_admin(&server).await;
        let root_id = my_id(&server, &root).await;
        let bob_id = add(&server, &root, BOB, "Bob").await;
        let path = user_path(&bob_id);
        let patch = |body: Value| call(&server, Method::PATCH, &path, Some(&root), Some(body));
        assert_eq!(patch(json!({"display_name": "Robert"})).await.0, 200);
        assert_eq!(patch(json!({"admin": true})).await.0, 200);
        let reset = json!({"password": "the reset password", "must_change": true});
        let reset_path = format!("{path}/password");
        assert_eq!(
            call(&server, Method::POST, &reset_path, Some(&root), Some(reset)).await.0,
            204
        );
        let change = json!({"upn": BOB, "current_password": "the reset password", "new_password": "bob's own password"});
        assert_eq!(call(&server, Method::POST, CHANGE, None, Some(change)).await.0, 204);
        cli(&server, &["user", "demote", BOB], "").await.unwrap();
        assert_eq!(call(&server, Method::DELETE, &path, Some(&root), None).await.0, 204);
        // refused actions leave no entry
        assert_eq!(
            call(&server, Method::DELETE, &user_path(&root_id), Some(&root), None)
                .await
                .0,
            409
        );

        let (status, audit) = call(&server, Method::GET, AUDIT, Some(&root), None).await;
        assert_eq!(status, 200, "{backend:?}: {audit}");
        let entries = audit["entries"].as_array().unwrap();
        let rows: Vec<(&str, &str, &str)> = entries
            .iter()
            .map(|entry| {
                (
                    entry["action"].as_str().unwrap(),
                    entry["actor"].as_str().unwrap(),
                    entry["target"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("user.delete", ROOT, BOB),
                ("user.update", "cli", BOB),
                ("user.password_change", BOB, BOB),
                ("user.password_reset", ROOT, BOB),
                ("user.update", ROOT, BOB),
                ("user.update", ROOT, BOB),
                ("user.add", ROOT, BOB),
                ("setup.first_admin", "setup", ROOT),
            ],
            "{backend:?}"
        );
        assert_eq!(entries[0]["actor_id"], root_id.as_str());
        assert_eq!(entries[0]["target_id"], bob_id.as_str());
        assert_eq!(entries[1]["actor_id"], Value::Null);
        assert_eq!(entries[1]["detail"], json!({"admin": false}));
        assert_eq!(entries[3]["detail"], json!({"must_change": true}));
        assert_eq!(entries[5]["detail"], json!({"display_name": "Robert"}));
        assert!(entries.iter().all(|entry| entry["at"].as_i64().unwrap() > 0));
        let text = audit.to_string();
        for secret in [PASSWORD, "the reset password", "bob's own password", "argon2"] {
            assert!(!text.contains(secret), "the audit holds no password");
        }

        // newest first, in pages
        let (_, first) = call(&server, Method::GET, &format!("{AUDIT}?limit=3"), Some(&root), None).await;
        assert_eq!(first["entries"].as_array().unwrap().len(), 3);
        let next = first["next"].as_i64().unwrap();
        let page = format!("{AUDIT}?limit=10&before={next}");
        let (_, rest) = call(&server, Method::GET, &page, Some(&root), None).await;
        assert_eq!(rest["entries"].as_array().unwrap().len(), 5);
        assert_eq!(rest["entries"][0]["action"], "user.password_reset");
        assert_eq!(rest["next"], Value::Null);
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_command_line_assigns_admins_and_guards_the_last_one() {
    let server = TestServer::start(Backend::Sqlite).await;
    let password = format!("{PASSWORD}\n");
    let added = cli(
        &server,
        &["user", "add", "ada@own.test", "--name", "Ada", "--admin"],
        &password,
    )
    .await
    .unwrap();
    assert!(added.starts_with("added ada@own.test ("), "{added}");
    cli(&server, &["user", "add", BOB, "--name", "Bob"], &password)
        .await
        .unwrap();
    // an admin made on the command line: no setup is needed, and no code is issued
    assert_eq!(
        call(&server, Method::GET, SETUP, None, None).await.1["needs_admin"],
        false
    );
    assert!(cli(&server, &["setup-code"], "").await.is_err());
    let roles = async || {
        let listed = cli(&server, &["user", "list"], "").await.unwrap();
        listed
            .lines()
            .map(|line| {
                let fields: Vec<&str> = line.split('\t').collect();
                format!("{} {} {}", fields[0], fields[2], fields[4])
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        roles().await,
        ["ada@own.test enabled admin", "bob@own.test enabled user"]
    );
    let ada = bearer(&server, "ada@own.test", PASSWORD).await;
    assert_eq!(call(&server, Method::GET, USERS, Some(&ada), None).await.0, 200);

    // the last admin stays, on the command line too
    for refused in [
        &["user", "demote", "ada@own.test"][..],
        &["user", "disable", "ada@own.test"][..],
        &["user", "delete", "ada@own.test"][..],
        &["user", "promote", "nobody@own.test"][..],
        &["user", "delete", "nobody@own.test"][..],
    ] {
        assert!(cli(&server, refused, "").await.is_err(), "{refused:?}");
    }
    cli(&server, &["user", "promote", BOB], "").await.unwrap();
    cli(&server, &["user", "demote", "ada@own.test"], "").await.unwrap();
    assert_eq!(
        roles().await,
        ["ada@own.test enabled user", "bob@own.test enabled admin"]
    );
    assert_eq!(call(&server, Method::GET, USERS, Some(&ada), None).await.0, 403);
    cli(&server, &["user", "delete", "ada@own.test"], "").await.unwrap();
    assert_eq!(roles().await, ["bob@own.test enabled admin"]);
    assert_eq!(
        server
            .password_grant("ada@own.test", PASSWORD, &Scope::spaces())
            .await
            .0,
        400
    );
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn two_nodes_share_the_setup_the_accounts_and_the_limits() {
    for backend in backends() {
        let a = TestServer::start_with(backend, |config| config.redis_url = redis_url()).await;
        let shared = a.app.config.clone();
        // one deployment: the same database, public origin and Redis
        let b = TestServer::start_with(Backend::Sqlite, |config| {
            config.database_url = shared.database_url.clone();
            config.public_origin = shared.public_origin.clone();
            config.redis_url = shared.redis_url.clone();
        })
        .await;

        // a code issued for the deployment is taken by any node
        let code = setup_code(&a).await;
        assert_eq!(
            call(&b, Method::GET, SETUP, None, None).await.1["needs_admin"],
            true
        );
        let (status, answer) = call(&b, Method::POST, SETUP, None, Some(first_account(Some(&code)))).await;
        assert_eq!(status, 201, "{backend:?}: {answer}");
        assert_eq!(
            call(&a, Method::GET, SETUP, None, None).await.1["needs_admin"],
            false
        );
        let (status, answer) = call(&a, Method::POST, SETUP, None, Some(first_account(Some(&code)))).await;
        assert_eq!((status, error_code(&answer)), (409, "AdminExists"));

        // an admin's work on one node holds on the other
        let root = bearer(&a, ROOT, PASSWORD).await;
        let bob_id = add(&a, &root, BOB, "Bob").await;
        let path = user_path(&bob_id);
        let (status, list) = call(&b, Method::GET, USERS, Some(&root), None).await;
        assert_eq!((status, upns(&list)), (200, vec![BOB, ROOT]), "{backend:?}");
        let session = b.session(BOB).await;
        let bob = bearer(&b, BOB, PASSWORD).await;
        let promote = Some(json!({"admin": true}));
        assert_eq!(call(&a, Method::PATCH, &path, Some(&root), promote).await.0, 200);
        assert_eq!(call(&b, Method::GET, AUDIT, Some(&bob), None).await.0, 200);
        assert_eq!(call(&b, Method::DELETE, &path, Some(&root), None).await.0, 204);
        assert_eq!(call(&a, Method::GET, USERS, Some(&bob), None).await.0, 403);
        let error = session.access_token(IC3_AUDIENCE).await.unwrap_err();
        assert!(matches!(error, TokenError::InvalidGrant(_)), "{backend:?}: {error:?}");

        // with Redis, setup attempts on either node count together
        if shared.redis_url.is_some() {
            for node in [&a, &b, &a, &b] {
                let again = first_account(Some(&code));
                assert_eq!(call(node, Method::POST, SETUP, None, Some(again)).await.0, 409);
            }
            for node in [&a, &b] {
                let again = first_account(Some(&code));
                let held = send(node, Method::POST, SETUP, None, Some(again)).await;
                assert_eq!(held.status(), 429, "{backend:?}");
                assert!(held.headers().contains_key("retry-after"));
            }
        }
        b.stop().await;
        a.stop().await;
    }
}
