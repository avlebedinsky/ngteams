//! Taking the authorization code from a recorded redirect `Location`: the `nativeclient` page and the custom-scheme
//! fallback, the `state` check, Entra's error answers, and URLs that only look like the redirect. No error may quote
//! the code or the state.

use teams_auth::redirect::{RedirectError, RedirectUri};
use teams_auth::token::{Authorization, EntraConfig, NATIVE_CLIENT_REDIRECT, Scope};

const ACCOUNT: &str = "a@lab.example.test";
const CUSTOM_SCHEME: &str = "msauth.com.microsoft.teams2://auth";
const CODE: &str = "1.AQ-made-up-code_value";
const STATE: &str = "s7ate-made-up-123456";

fn authorization(redirect_uri: &str) -> Authorization {
    let config = EntraConfig::teams_native().with_redirect_uri(redirect_uri);
    let pkce = teams_auth::pkce::Pkce::from_bytes(&[7; 32]);
    Authorization::with_pkce(&config, ACCOUNT, Scope::spaces(), pkce, STATE.to_owned())
}

fn native() -> Authorization {
    authorization(NATIVE_CLIENT_REDIRECT)
}

/// The `Location` Entra answers the authorize request with: the redirect URI and a query.
fn location(redirect_uri: &str, query: &str) -> String {
    format!("{redirect_uri}?{query}")
}

fn assert_quiet(error: &RedirectError) {
    let shown = format!("{error} {error:?}");
    assert!(!shown.contains(CODE), "the code leaked: {shown}");
    assert!(!shown.contains(STATE), "the state leaked: {shown}");
}

#[test]
fn the_code_is_taken_from_the_nativeclient_redirect() {
    let url = location(
        NATIVE_CLIENT_REDIRECT,
        &format!("code={CODE}&state={STATE}&session_state=x"),
    );
    let code = native().code_from_redirect(&url).expect("a code");
    assert_eq!(code.expose(), CODE);
    assert!(!format!("{code:?}").contains(CODE), "Debug hides the code");
}

#[test]
fn the_code_is_taken_from_the_custom_scheme_redirect() {
    let url = location(CUSTOM_SCHEME, &format!("code={CODE}&state={STATE}"));
    let code = authorization(CUSTOM_SCHEME).code_from_redirect(&url).expect("a code");
    assert_eq!(code.expose(), CODE);
}

#[test]
fn percent_encoded_values_are_decoded() {
    let url = location(NATIVE_CLIENT_REDIRECT, "code=a%2Bb%3D&state=s7ate-made-up-123456");
    assert_eq!(native().code_from_redirect(&url).expect("a code").expose(), "a+b=");
}

#[test]
fn a_redirect_for_another_request_is_refused() {
    let cases = [
        (format!("code={CODE}&state=someone-elses"), "mismatch"),
        (format!("code={CODE}"), "missing"),
        (format!("code={CODE}&state="), "mismatch"),
        (format!("code={CODE}&state={STATE}&state={STATE}"), "repeated"),
    ];
    for (query, expected) in cases {
        let error = native()
            .code_from_redirect(&location(NATIVE_CLIENT_REDIRECT, &query))
            .expect_err(&query);
        let kind = match error {
            RedirectError::StateMismatch => "mismatch",
            RedirectError::MissingState => "missing",
            RedirectError::Repeated("state") => "repeated",
            ref other => panic!("{query}: {other:?}"),
        };
        assert_eq!(kind, expected, "{query}");
        assert_quiet(&error);
    }
}

#[test]
fn the_answer_must_be_in_the_query() {
    let url = format!("{NATIVE_CLIENT_REDIRECT}#code={CODE}&state={STATE}");
    let error = native().code_from_redirect(&url).expect_err("fragment only");
    assert!(matches!(error, RedirectError::MissingState), "{error:?}");
}

#[test]
fn a_missing_empty_or_repeated_code_gives_no_code() {
    for query in [
        format!("state={STATE}"),
        format!("code=&state={STATE}"),
        format!("code={CODE}&code=other&state={STATE}"),
    ] {
        let error = native()
            .code_from_redirect(&location(NATIVE_CLIENT_REDIRECT, &query))
            .expect_err(&query);
        assert!(
            matches!(error, RedirectError::MissingCode | RedirectError::Repeated("code")),
            "{query}: {error:?}"
        );
        assert_quiet(&error);
    }
}

#[test]
fn an_error_redirect_is_entras_refusal_without_its_trace() {
    let query = format!(
        "error=interaction_required&error_description=AADSTS50076%3A+Due+to+a+configuration+change+you+must+use+\
         multi-factor+authentication.+Trace+ID%3A+0000+Correlation+ID%3A+1111&state={STATE}"
    );
    let error = native()
        .code_from_redirect(&location(NATIVE_CLIENT_REDIRECT, &query))
        .expect_err("refused");
    let RedirectError::Refused(ref entra) = error else {
        panic!("{error:?}");
    };
    assert_eq!(entra.error, "interaction_required");
    assert_eq!(entra.codes, [50_076]);
    assert!(
        entra.description.starts_with("Due to a configuration change"),
        "{entra:?}"
    );
    assert!(!entra.description.contains("Trace ID"), "{entra:?}");
    assert_eq!(
        error.to_string(),
        "Entra refused the authorization: interaction_required AADSTS50076: Due to a configuration change you must \
         use multi-factor authentication."
    );
    assert_quiet(&error);
}

#[test]
fn an_error_with_the_wrong_state_is_a_state_mismatch() {
    let query = "error=access_denied&state=someone-elses";
    let error = native()
        .code_from_redirect(&location(NATIVE_CLIENT_REDIRECT, query))
        .expect_err("mismatch");
    assert!(matches!(error, RedirectError::StateMismatch), "{error:?}");
}

#[test]
fn urls_that_only_look_like_the_redirect_are_refused() {
    let query = format!("code={CODE}&state={STATE}");
    let lookalikes = [
        "http://login.microsoftonline.com/common/oauth2/nativeclient",
        "https://login.microsoftonline.com/common/oauth2/nativeclient/",
        "https://login.microsoftonline.com/common/oauth2/nativeclient2",
        "https://login.microsoftonline.com/organizations/oauth2/nativeclient",
        "https://login.microsoftonline.com:8443/common/oauth2/nativeclient",
        "https://user@login.microsoftonline.com/common/oauth2/nativeclient",
        "https://login.microsoftonline.com.attacker.example/common/oauth2/nativeclient",
        "https://attacker.example/common/oauth2/nativeclient",
        "msauth.com.microsoft.teams2://auth",
        "not a url",
    ];
    for lookalike in lookalikes {
        let error = native()
            .code_from_redirect(&location(lookalike, &query))
            .expect_err(lookalike);
        assert!(matches!(error, RedirectError::NotTheRedirect), "{lookalike}: {error:?}");
        assert_quiet(&error);
    }
    let error = authorization(CUSTOM_SCHEME)
        .code_from_redirect(&location("msauth.com.microsoft.teams2://other", &query))
        .expect_err("another host");
    assert!(matches!(error, RedirectError::NotTheRedirect), "{error:?}");
}

#[test]
fn the_default_port_is_the_same_redirect() {
    let url = location(
        "https://login.microsoftonline.com:443/common/oauth2/nativeclient",
        &format!("code={CODE}&state={STATE}"),
    );
    assert!(native().code_from_redirect(&url).is_ok());
}

#[test]
fn a_redirect_uri_is_a_bare_absolute_url() {
    for good in [NATIVE_CLIENT_REDIRECT, CUSTOM_SCHEME, "http://127.0.0.1:4000/cb"] {
        let uri = RedirectUri::parse(good).expect(good);
        assert!(uri.matches(&format!("{good}?code=1")), "{good}");
    }
    for bad in [
        "https://login.microsoftonline.com/cb?x=1",
        "https://login.microsoftonline.com/cb#x",
        "https://user@login.microsoftonline.com/cb",
        "urn:ietf:wg:oauth:2.0:oob",
        "relative/path",
    ] {
        assert!(
            matches!(RedirectUri::parse(bad), Err(RedirectError::InvalidRedirectUri)),
            "{bad}"
        );
    }
}
