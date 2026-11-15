//! An own server's account management (`teams_auth::admin`) against `wiremock`: the requests the server gets, what
//! its answers and refusals become, and that no password, setup code or token is kept in an error. Every value is
//! made up; the answers have the shapes `docs/server.md` gives.

use std::time::Duration;

use serde_json::{Value, json};
use teams_auth::admin::{
    AdminClient, AdminError, AdminUser, FirstAdmin, NewUser, Refusal, SetupState, UserChanges, suggested_password,
};
use teams_auth::cloud::Cloud;
use teams_auth::secrets::Secret;
use wiremock::matchers::{any, body_json, header, method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TOKEN: &str = "made-up-access-token";
const ID: &str = "00000000-0000-4000-8000-0000000000d1";

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn user(upn: &str, admin: bool) -> Value {
    json!({
        "id": ID,
        "mri": format!("8:orgid:{ID}"),
        "upn": upn,
        "display_name": "Dana Doe",
        "enabled": true,
        "admin": admin,
        "must_change_password": false,
        "created_at": 1,
        "updated_at": 2,
    })
}

fn refusal(status: u16, code: &str, message: &str) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_json(json!({"error": {"code": code, "message": message}}))
}

async fn server() -> TestResult<(MockServer, AdminClient)> {
    let server = MockServer::start().await;
    let client = AdminClient::with_loopback(&server.uri())?;
    Ok((server, client))
}

#[tokio::test]
async fn the_setup_state_is_asked_without_a_token() -> TestResult {
    let (server, client) = server().await?;
    Mock::given(method("GET"))
        .and(path("/api/admin/v1/setup"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"needs_admin": true, "code_required": true})))
        .expect(1)
        .mount(&server)
        .await;
    let state = client.setup_state().await?;
    assert_eq!(
        state,
        SetupState {
            needs_admin: true,
            code_required: true
        }
    );
    let asked = &server.received_requests().await.ok_or("requests")?[0];
    assert!(!asked.headers.contains_key("authorization"));
    Ok(())
}

#[tokio::test]
async fn an_answer_that_doesnt_say_is_refused() -> TestResult {
    let (server, client) = server().await?;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"needs_admin": true})))
        .mount(&server)
        .await;
    assert!(matches!(
        client.setup_state().await,
        Err(AdminError::UnexpectedResponse { status: 200 })
    ));
    Ok(())
}

#[tokio::test]
async fn the_first_administrator_is_created_with_the_setup_code() -> TestResult {
    let (server, client) = server().await?;
    Mock::given(method("POST"))
        .and(path("/api/admin/v1/setup"))
        .and(body_json(json!({
            "code": "AAAAA-BBBBB",
            "upn": "admin@example.test",
            "display_name": "Ada Admin",
            "password": "made-up password",
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(user("admin@example.test", true)))
        .expect(1)
        .mount(&server)
        .await;
    let (code, password) = (Secret::new("AAAAA-BBBBB"), Secret::new("made-up password"));
    let created = client
        .create_first_admin(FirstAdmin {
            code: Some(&code),
            upn: "admin@example.test",
            display_name: "Ada Admin",
            password: &password,
        })
        .await?;
    assert!(created.admin && created.enabled);
    assert_eq!(created.id, ID);
    Ok(())
}

#[tokio::test]
async fn a_server_that_asks_no_code_gets_none() -> TestResult {
    let (server, client) = server().await?;
    Mock::given(method("POST"))
        .and(body_json(json!({
            "upn": "admin@example.test",
            "display_name": "",
            "password": "made-up password",
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(user("admin@example.test", true)))
        .expect(1)
        .mount(&server)
        .await;
    let password = Secret::new("made-up password");
    client
        .create_first_admin(FirstAdmin {
            code: None,
            upn: "admin@example.test",
            display_name: "",
            password: &password,
        })
        .await?;
    Ok(())
}

#[tokio::test]
async fn refusals_are_told_apart_and_carry_no_secret() -> TestResult {
    let cases = [
        (403, "SetupCode", Refusal::SetupCode),
        (409, "AdminExists", Refusal::AdminExists),
        (403, "InvalidCredentials", Refusal::InvalidCredentials),
        (400, "WeakPassword", Refusal::WeakPassword),
        (400, "SamePassword", Refusal::SamePassword),
        (400, "InvalidUpn", Refusal::InvalidUpn),
        (400, "InvalidName", Refusal::InvalidName),
        (409, "UpnTaken", Refusal::UpnTaken),
        (409, "LastAdmin", Refusal::LastAdmin),
        (404, "NotFound", Refusal::NotFound),
        (403, "NotAdmin", Refusal::NotAdmin),
        (403, "AccountDisabled", Refusal::AccountDisabled),
        (500, "ServerError", Refusal::Other),
    ];
    let password = Secret::new("made-up password");
    for (status, code, wanted) in cases {
        let (server, client) = server().await?;
        Mock::given(method("POST"))
            .respond_with(refusal(status, code, "no: made-up password was sent"))
            .mount(&server)
            .await;
        let error = client
            .change_password("dana@example.test", &password, &Secret::new("another made-up one"))
            .await
            .err()
            .ok_or("a refusal")?;
        assert_eq!(error.refusal(), Some(wanted), "{code}");
        let said = format!("{error} {error:?}");
        assert!(!said.contains("made-up password"), "{said}");
        assert!(said.contains(&status.to_string()), "{said}");
    }
    Ok(())
}

#[tokio::test]
async fn a_password_is_changed_by_user_name_and_current_password() -> TestResult {
    let (server, client) = server().await?;
    Mock::given(method("POST"))
        .and(path("/api/account/v1/password"))
        .and(body_json(json!({
            "upn": "dana@example.test",
            "current_password": "the given one",
            "new_password": "the chosen one",
        })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    client
        .change_password(
            "dana@example.test",
            &Secret::new("the given one"),
            &Secret::new("the chosen one"),
        )
        .await?;
    Ok(())
}

#[tokio::test]
async fn too_many_attempts_say_how_long_to_wait() -> TestResult {
    let (server, client) = server().await?;
    Mock::given(method("POST"))
        .respond_with(refusal(429, "TooManyAttempts", "later").insert_header("retry-after", "120"))
        .mount(&server)
        .await;
    let error = client
        .change_password("dana@example.test", &Secret::new("a"), &Secret::new("b"))
        .await
        .err()
        .ok_or("a refusal")?;
    assert!(
        matches!(error, AdminError::Throttled { retry_after: Some(wait) } if wait == Duration::from_secs(120)),
        "{error:?}"
    );
    assert_eq!(error.refusal(), None);
    Ok(())
}

#[tokio::test]
async fn the_account_itself_is_read_with_its_token() -> TestResult {
    let (server, client) = server().await?;
    Mock::given(method("GET"))
        .and(path("/api/account/v1/me"))
        .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(user("admin@example.test", true)))
        .expect(1)
        .mount(&server)
        .await;
    let me = client.me(&Secret::new(TOKEN)).await?;
    assert_eq!(
        me,
        AdminUser {
            id: ID.to_owned(),
            upn: "admin@example.test".to_owned(),
            display_name: "Dana Doe".to_owned(),
            enabled: true,
            admin: true,
            must_change_password: false,
        }
    );
    Ok(())
}

#[tokio::test]
async fn a_refused_token_is_unauthorized() -> TestResult {
    let (server, client) = server().await?;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401).insert_header("www-authenticate", "Bearer"))
        .mount(&server)
        .await;
    assert!(matches!(
        client.me(&Secret::new(TOKEN)).await,
        Err(AdminError::Unauthorized)
    ));
    let (server, client) = self::server().await?;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(502).set_body_string("bad gateway"))
        .mount(&server)
        .await;
    assert!(matches!(
        client.me(&Secret::new(TOKEN)).await,
        Err(AdminError::UnexpectedResponse { status: 502 })
    ));
    Ok(())
}

#[tokio::test]
async fn accounts_are_listed_by_page_and_search() -> TestResult {
    let (server, client) = server().await?;
    Mock::given(method("GET"))
        .and(path("/api/admin/v1/users"))
        .and(query_param("limit", "2"))
        .and(query_param_is_missing("q"))
        .and(query_param_is_missing("after"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "users": [user("a@example.test", true), user("b@example.test", false)],
            "next": "b@example.test",
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/admin/v1/users"))
        .and(query_param("q", "da na"))
        .and(query_param("after", "b@example.test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"users": [], "next": null})))
        .expect(1)
        .mount(&server)
        .await;
    let token = Secret::new(TOKEN);
    let first = client.users(&token, "", None, 2).await?;
    assert_eq!(first.users.len(), 2);
    assert_eq!(first.next.as_deref(), Some("b@example.test"));
    let second = client.users(&token, "da na", first.next.as_deref(), 2).await?;
    assert!(second.users.is_empty() && second.next.is_none());
    Ok(())
}

#[tokio::test]
async fn a_list_in_another_form_is_refused() -> TestResult {
    let (server, client) = server().await?;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"users": [{"id": ID}]})))
        .mount(&server)
        .await;
    assert!(matches!(
        client.users(&Secret::new(TOKEN), "", None, 50).await,
        Err(AdminError::UnexpectedResponse { status: 200 })
    ));
    Ok(())
}

#[tokio::test]
async fn a_success_that_names_no_account_is_refused() -> TestResult {
    let (server, client) = server().await?;
    // A success whose body is neither an account (no `upn`, no flags) nor a list: each call that reads one takes it
    // as an unexpected answer with the status it got.
    Mock::given(any())
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": ID, "ok": true})))
        .mount(&server)
        .await;
    let (token, password) = (Secret::new(TOKEN), Secret::new("a made-up password"));
    let unexpected = |result: Result<AdminUser, AdminError>| -> bool {
        matches!(result, Err(AdminError::UnexpectedResponse { status: 201 }))
    };
    assert!(unexpected(
        client
            .create_first_admin(FirstAdmin {
                code: None,
                upn: "dana@example.test",
                display_name: "Dana Doe",
                password: &password,
            })
            .await
    ));
    assert!(unexpected(client.me(&token).await));
    assert!(unexpected(
        client
            .create_user(
                &token,
                NewUser {
                    upn: "dana@example.test",
                    display_name: "Dana Doe",
                    password: &password,
                    admin: false,
                    must_change_password: true,
                },
            )
            .await
    ));
    let rename = UserChanges {
        display_name: Some("Dana D.".to_owned()),
        ..UserChanges::default()
    };
    assert!(unexpected(client.update_user(&token, ID, &rename).await));
    assert!(matches!(
        client.users(&token, "", None, 50).await,
        Err(AdminError::UnexpectedResponse { status: 201 })
    ));
    Ok(())
}

/// The server's answers to each change of [`an_account_is_added_changed_reset_and_deleted`], each expected once.
async fn mount_changes(server: &MockServer) {
    let users_id = format!("/api/admin/v1/users/{ID}");
    Mock::given(method("POST"))
        .and(path("/api/admin/v1/users"))
        .and(body_json(json!({
            "upn": "dana@example.test",
            "display_name": "Dana Doe",
            "password": "the given one",
            "admin": false,
            "must_change_password": true,
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(user("dana@example.test", false)))
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("PATCH"))
        .and(path(users_id.as_str()))
        .and(body_json(json!({"admin": true})))
        .respond_with(ResponseTemplate::new(200).set_body_json(user("dana@example.test", true)))
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("PATCH"))
        .and(path(users_id.as_str()))
        .and(body_json(
            json!({"display_name": "D", "upn": "d@example.test", "enabled": false}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(user("d@example.test", true)))
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("{users_id}/password").as_str()))
        .and(body_json(
            json!({"password": "another given one", "must_change_password": true}),
        ))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(users_id.as_str()))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn an_account_is_added_changed_reset_and_deleted() -> TestResult {
    let (server, client) = server().await?;
    mount_changes(&server).await;
    let (token, given) = (Secret::new(TOKEN), Secret::new("the given one"));
    let added = client
        .create_user(
            &token,
            NewUser {
                upn: "dana@example.test",
                display_name: "Dana Doe",
                password: &given,
                admin: false,
                must_change_password: true,
            },
        )
        .await?;
    assert!(!added.admin);
    let promote = UserChanges {
        admin: Some(true),
        ..UserChanges::default()
    };
    assert!(!promote.is_empty() && UserChanges::default().is_empty());
    assert!(client.update_user(&token, ID, &promote).await?.admin);
    let renamed = UserChanges {
        display_name: Some("D".to_owned()),
        upn: Some("d@example.test".to_owned()),
        enabled: Some(false),
        admin: None,
    };
    assert_eq!(client.update_user(&token, ID, &renamed).await?.upn, "d@example.test");
    client
        .reset_password(&token, ID, &Secret::new("another given one"), true)
        .await?;
    client.delete_user(&token, ID).await?;
    for request in server.received_requests().await.ok_or("requests")? {
        assert_eq!(
            request
                .headers
                .get("authorization")
                .and_then(|value| value.to_str().ok()),
            Some(format!("Bearer {TOKEN}").as_str())
        );
    }
    Ok(())
}

#[tokio::test]
async fn an_id_cant_leave_its_path_segment() -> TestResult {
    let (server, client) = server().await?;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    client.delete_user(&Secret::new(TOKEN), "../setup?x=1").await?;
    let asked = &server.received_requests().await.ok_or("requests")?[0];
    assert_eq!(asked.url.path(), "/api/admin/v1/users/..%2Fsetup%3Fx=1");
    assert_eq!(asked.url.query(), None);
    Ok(())
}

#[tokio::test]
async fn a_server_that_isnt_there_is_an_http_error() -> TestResult {
    // a port nothing listens on any more
    let port = std::net::TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port();
    let client = AdminClient::with_loopback(&format!("http://127.0.0.1:{port}"))?;
    let error = client.setup_state().await.err().ok_or("no answer")?;
    assert!(matches!(error, AdminError::Http(_)), "{error:?}");
    assert_eq!(error.refusal(), None);
    Ok(())
}

#[test]
fn only_an_own_server_has_account_management() -> TestResult {
    assert!(matches!(
        AdminClient::for_cloud(&Cloud::Microsoft),
        Err(AdminError::NotOwnServer)
    ));
    assert!(AdminClient::for_cloud(&Cloud::own("https://chat.example.test:8443")?).is_ok());
    for origin in [
        "https://127.0.0.1:1",
        "http://chat.example.test",
        "http://127.0.0.1:1/path",
        "http://127.0.0.1:1/?q=1",
        "not a url",
    ] {
        assert!(
            matches!(AdminClient::with_loopback(origin), Err(AdminError::InvalidOrigin)),
            "{origin}"
        );
    }
    assert!(AdminClient::with_loopback("http://[::1]:1").is_ok());
    Ok(())
}

#[test]
fn a_suggested_password_is_long_and_new_each_time() -> TestResult {
    let (first, second) = (suggested_password()?, suggested_password()?);
    assert_ne!(first, second);
    for password in [first, second] {
        let groups: Vec<&str> = password.split('-').collect();
        assert_eq!(groups.len(), 4);
        assert!(
            groups
                .iter()
                .all(|group| group.len() == 5 && group.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()))
        );
    }
    Ok(())
}
