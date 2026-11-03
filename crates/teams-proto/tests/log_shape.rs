//! Synthetic endpoint/path-shape cases moved with the shared URL helpers.

use teams_proto::{log_url, path_shape};

const PLANTED_TOKEN: &str = "eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiJwbGFudGVkLXN1YmplY3QifQ.c2lnbmF0dXJlLXZhbHVlLXBsYW50ZWQ";

#[test]
fn a_path_shape_names_the_endpoint_with_every_id_replaced_by_its_kind() {
    let cases = [
        ("/", "/"),
        ("", ""),
        (
            "/api/chatsvc/emea/v1/users/ME/conversations/19%3Aabc_def%40thread.v2/messages/1760000000123/properties",
            "/api/chatsvc/emea/v1/users/ME/conversations/<chat>/messages/<message>/properties",
        ),
        (
            "/v1/users/8%3Aorgid%3A00000000-0000-4000-8000-00000000000a/endpoints/%7B00000000-0000-4000-8000-0000000000ef%7D",
            "/v1/users/<mri>/endpoints/<guid>",
        ),
        (
            "/v1/users/ME/conversations/48%3Anotifications/messages/1760000000123/properties",
            "/v1/users/ME/conversations/48:notifications/messages/<message>/properties",
        ),
        (
            "/v1/users/ME/conversations/48%3Anotes2",
            "/v1/users/ME/conversations/<mri>",
        ),
        (
            "/v1/threads/19:x@thread.tacv2/members/28:bot",
            "/v1/threads/<chat>/members/<mri>",
        ),
        (
            "/v1/objects/0-weu-d3-0123456789abcdef0123456789abcdef/content/imgpsh",
            "/v1/objects/<id>/content/imgpsh",
        ),
        (
            "/api/mt/part/amer-01/beta/users/00000000-0000-4000-8000-00000000000a/automaticRepliesSetting",
            "/api/mt/part/amer-01/beta/users/<guid>/automaticRepliesSetting",
        ),
        ("/v1/presence/abc1234567/state", "/v1/presence/<id>/state"),
        ("/v1/a%20b/c=d/x.y_z~$-w", "/v1/<id>/<id>/x.y_z~$-w"),
        ("/v1/2/12345/123456", "/v1/2/12345/<message>"),
        ("/v1/aVeryLongSegmentThatIsMoreThanThirtyTwoChars", "/v1/<id>"),
        ("/v1/%FF%FE", "/v1/<id>"),
    ];
    for (path, shape) in cases {
        assert_eq!(path_shape(path), shape, "{path}");
    }
}

#[test]
fn a_logged_url_keeps_the_endpoint_and_the_query_s_keys_never_a_credential() {
    let t = PLANTED_TOKEN;
    let cases = [
        // a call follow-up as the calling services hand it out after a meeting join
        (
            format!(
                "https://api.flightproxy.teams.microsoft.com/api/v2/ep/conv.skype.com/conv/c1/leave?i=1&e=2&skypeToken={t}"
            ),
            "https://api.flightproxy.teams.microsoft.com/api/v2/ep/conv.skype.com/conv/c1/leave?i&e&skypeToken"
                .to_owned(),
        ),
        // query credentials, listed or not, and a fragment's
        (
            format!(
                "https://host/a?access_token={t}&sig=s1g&token=t0k&key=k3y&sv=2020&unlisted=s3cret#access_token={t}"
            ),
            "https://host/a?access_token&sig&token&key&sv&unlisted".to_owned(),
        ),
        // user info and a non-default port
        (
            format!("wss://user:{t}@host:8443/v4/c"),
            "wss://host:8443/v4/c".to_owned(),
        ),
        // a token in the path: JWT-shaped, after a credential's name, a matrix parameter, a long opaque run
        (
            format!("https://host/v1/{t}/x"),
            "https://host/v1/[redacted]/x".to_owned(),
        ),
        (
            "https://host/v1/skypetoken/s3cr3t/x".to_owned(),
            "https://host/v1/skypetoken/[redacted]/x".to_owned(),
        ),
        (
            "https://host/v1/obj;sig=s3cr3t/x".to_owned(),
            "https://host/v1/obj;sig=[redacted]/x".to_owned(),
        ),
        (
            format!("https://host/v1/{}", "a".repeat(120)),
            "https://host/v1/[redacted]".to_owned(),
        ),
        // a token as a query key without a value
        (format!("https://host/a?{t}"), "https://host/a?[redacted]".to_owned()),
        // ids and words of the API stay
        (
            "https://host/v1/users/ME/conversations/19:a_b@unq.gbl.spaces/messages".to_owned(),
            "https://host/v1/users/ME/conversations/19:a_b@unq.gbl.spaces/messages".to_owned(),
        ),
        (
            "https://us-api.asm.skype.com/v1/skypetokenauth".to_owned(),
            "https://us-api.asm.skype.com/v1/skypetokenauth".to_owned(),
        ),
        // what has no path to keep, or can't be read
        (format!("data:text/plain,{t}"), "[unreadable url]".to_owned()),
        (format!("not a url {t}"), "[unreadable url]".to_owned()),
        (String::new(), String::new()),
        (format!("?tc={t}"), String::new()),
    ];
    for (url, expected) in &cases {
        let logged = log_url(url);
        assert_eq!(&logged, expected);
        assert!(!logged.contains("eyJ") && !logged.contains("s3cr"), "{logged}");
    }
}

#[test]
fn signin_path_credentials_use_the_loggers_shared_names_too() {
    for name in [
        "code",
        "code_verifier",
        "refresh_token",
        "CLIENT_SECRET",
        "Cookie",
        "signature",
    ] {
        let url = format!("https://example.test/{name}/made-up-credential?unknown=made-up-query#made-up-fragment");
        assert_eq!(log_url(&url), format!("https://example.test/{name}/[redacted]?unknown"));
    }
    assert_eq!(log_url("login.example.test:443"), "login.example.test:443");
    assert_eq!(log_url("[::1]:8080"), "[::1]:8080");
    assert_eq!(log_url("#made-up-fragment"), "");
}
