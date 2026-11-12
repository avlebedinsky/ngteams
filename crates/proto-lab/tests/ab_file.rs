//! The `ab-file` scenario's offline parts: its forced denies take every write to the file services and AMS objects on
//! any path, and every write naming a lab file, and no read; its deny policy expects those and the probes only; its
//! texts and file are its own.

// The full Linux browser/scenario harness is outside the Windows provisioning command set.
#![cfg(unix)]

use proto_lab::capture_hold::{HeldRequest, PausedRequest};
use proto_lab::scenario::ScenarioName;
use proto_lab::scenario::ab_file::{self, FILE_MARKER, FILE_MESSAGE, FILE_WRITE, Texts, file_rules, test_file};
use proto_lab::scenario::ab_image::MEDIA_WRITE;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn request(method: &str, url: &str, body: &str) -> PausedRequest {
    PausedRequest {
        request_id: "1".to_owned(),
        method: method.to_owned(),
        url: url.to_owned(),
        body: Some(body.as_bytes().to_vec()),
        network_id: None,
    }
}

/// The label of the first rule of `file_rules` that denies the request, as the fence names a deny.
fn label(method: &str, url: &str, body: &str) -> TestResult<Option<String>> {
    let request = request(method, url, body);
    Ok(file_rules()?
        .iter()
        .find(|rule| rule.matches(&request))
        .and_then(|rule| rule.label().map(str::to_owned)))
}

#[test]
fn every_write_to_a_file_service_is_forced_denied_on_any_path_and_no_read() -> TestResult {
    for (method, url) in [
        (
            "POST",
            "https://contoso-my.sharepoint.com/personal/a_contoso_com/_api/v2.0/drive/root:/Microsoft%20Teams%20Chat%20Files/x.txt:/createUploadSession",
        ),
        (
            "PUT",
            "https://contoso-my.sharepoint.com/personal/a_contoso_com/_api/v2.0/drive/items/01X/uploadSession?guid=x",
        ),
        ("POST", "https://contoso.sharepoint.com/sites/x/_api/web/lists"),
        (
            "PATCH",
            "https://CONTOSO-MY.SHAREPOINT.COM/_api/v2.1/drives/b!x/items/01X",
        ),
        ("DELETE", "https://contoso-my.sharepoint.com/_api/v2.0/drive/items/01X"),
        ("MERGE", "https://contoso-my.sharepoint.com/_api/web/x"),
        ("POST", "https://graph.microsoft.com/v1.0/me/drive/items/01X/invite"),
        ("POST", "https://graph.microsoft.com/v1.0/$batch"),
        ("POST", "https://graph.microsoft.com/beta/shares/u!x/permission/grant"),
        ("PUT", "https://my.microsoftpersonalcontent.com/personal/x/_api/x"),
        ("POST", "https://api.onedrive.com/v1.0/drive/items/x/action.createLink"),
        ("POST", "https://southcentralus1-mediap.svc.ms/transform/x"),
        ("POST", "https://contoso-my.sharepoint.com:443/_api/x"),
        ("POST", "https://contoso-my.sharepoint.com?x=1"),
        ("POST", "http://contoso-my.sharepoint.com/_api/x"),
    ] {
        assert_eq!(label(method, url, "{}")?.as_deref(), Some(FILE_WRITE), "{method} {url}");
    }
    // reads go on; so do other hosts that only end like one, or name one in their path or query
    for (method, url) in [
        ("GET", "https://contoso-my.sharepoint.com/_api/v2.0/drive/root"),
        ("OPTIONS", "https://contoso-my.sharepoint.com/_api/v2.0/drive/root"),
        ("GET", "https://graph.microsoft.com/v1.0/me/drive"),
        ("POST", "https://notsharepoint.com/_api/x"),
        ("POST", "https://contoso-my.sharepoint.com.example.org/_api/x"),
        ("POST", "https://graph.microsoft.com.example.org/v1.0/x"),
        ("POST", "https://example.org/sharepoint.com/x"),
        ("POST", "https://example.org/?u=https://graph.microsoft.com/"),
        (
            "POST",
            "https://teams.microsoft.com/api/chatsvc/amer/v1/users/ME/conversations/x/messages",
        ),
    ] {
        assert_eq!(label(method, url, "{}")?, None, "{method} {url}");
    }
    Ok(())
}

#[test]
fn a_write_to_an_ams_object_is_the_media_rule_s_and_a_write_naming_a_lab_file_is_denied_anywhere() -> TestResult {
    assert_eq!(
        label("POST", "https://us-prod.asyncgw.teams.microsoft.com/v1/objects/", "{}")?.as_deref(),
        Some(MEDIA_WRITE)
    );
    let post = "https://teams.microsoft.com/api/chatsvc/amer/v1/users/ME/conversations/x/messages";
    let texts = Texts::new("n1");
    for body in [
        format!(r#"{{"content":"<p>{}</p>"}}"#, texts.caption),
        format!(
            r#"{{"properties":{{"files":"[{{\"fileName\":\"{}\"}}]"}}}}"#,
            texts.file_name
        ),
        // an earlier run's file, sent again on a later load
        format!(r#"{{"content":"{}"}}"#, Texts::new("n0").caption),
    ] {
        assert_eq!(label("POST", post, &body)?.as_deref(), Some(FILE_MESSAGE), "{body}");
        assert_eq!(label("PUT", post, &body)?.as_deref(), Some(FILE_MESSAGE), "{body}");
        assert_eq!(
            label("POST", "https://teams.microsoft.com/api/mt/x", &body)?.as_deref(),
            Some(FILE_MESSAGE),
            "{body}"
        );
    }
    // the file service's rule comes first, whatever the body
    let body = format!(r#"{{"item":{{"name":"{}"}}}}"#, texts.file_name);
    assert_eq!(
        label("POST", "https://contoso-my.sharepoint.com/_api/x", &body)?.as_deref(),
        Some(FILE_WRITE)
    );
    // the plain message, a read and a delete go on
    let plain = format!(r#"{{"content":"<p>{}</p>"}}"#, texts.plain);
    assert_eq!(label("POST", post, &plain)?, None);
    assert_eq!(label("GET", post, &texts.caption)?, None);
    assert_eq!(label("DELETE", post, &texts.caption)?, None);
    Ok(())
}

#[test]
fn the_texts_and_file_are_this_runs_and_only_the_file_s_carry_the_marker() {
    let texts = Texts::new("n1");
    assert!(texts.plain.contains("n1") && texts.caption.contains("n1") && texts.file_name.contains("n1"));
    assert!(texts.caption.contains(FILE_MARKER) && texts.file_name.contains(FILE_MARKER));
    assert!(!texts.plain.contains(FILE_MARKER));
    assert_eq!(texts.file_stem(), "lab-file-n1");
    assert_eq!(std::path::Path::new(&texts.file_name).extension(), Some("txt".as_ref()));
    let content = String::from_utf8(test_file("n1")).unwrap_or_default();
    assert!(content.contains("n1") && content.contains("made-up"));
    assert!(
        !content.contains(FILE_MARKER),
        "the file's content is no write's marker"
    );
    assert_ne!(test_file("n2"), test_file("n1"));
    assert_eq!(ScenarioName::AbFile.as_str(), "ab-file");
}

#[test]
fn file_hold_records_are_identified_by_their_label() {
    let mut request = HeldRequest::new("POST", "https://synthetic.sharepoint.com/file");
    assert!(!ab_file::is_file_write(&request));
    request.label = Some(FILE_WRITE.into());
    assert!(ab_file::is_file_write(&request));
}
