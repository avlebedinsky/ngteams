use std::path::Path;

use teams_wpe::{SchemeError, SchemeResponse};

use super::Result;

pub(super) const ONE: &str = "synthetic://fixture/one";
pub(super) const TWO: &str = "synthetic://fixture/two";

/// A base for loaded HTML that is a secure context (a page on the synthetic scheme is not one); nothing is fetched
/// from it.
pub(super) const SECURE: &str = "http://localhost/synthetic-secure";

pub(super) fn allowed(uri: &str) -> bool {
    uri.starts_with("synthetic://fixture/") || uri == "about:blank" || uri == SECURE
}

pub(super) fn response(uri: &str, root: &Path) -> Result<SchemeResponse> {
    let html = match uri {
        ONE => HTML,
        TWO => "<!doctype html><title>two</title><body>synthetic second page</body>",
        "synthetic://fixture/file" => {
            let path = root.join("synthetic-page.html");
            std::fs::write(&path, "<!doctype html><title>file</title><body>synthetic file</body>")?;
            return Ok(SchemeResponse::File(path));
        }
        "synthetic://fixture/missing-file" => return Ok(SchemeResponse::File(root.join("absent.html"))),
        "synthetic://fixture/directory" => return Ok(SchemeResponse::File(root.to_path_buf())),
        other => {
            let kind = match other.rsplit('/').next().unwrap_or("") {
                "not-found" => SchemeError::NotFound,
                "permission" => SchemeError::PermissionDenied,
                "is-directory" => SchemeError::IsDirectory,
                _ => SchemeError::Failed,
            };
            return Ok(SchemeResponse::Error {
                kind,
                message: "synthetic scheme error".into(),
            });
        }
    };
    Ok(SchemeResponse::Bytes {
        data: html.as_bytes().to_vec(),
        mime: "text/html".into(),
    })
}

pub(super) const HTML: &str = r#"<!doctype html><html><head><meta charset="utf-8"><title>one</title>
<style>body {margin:12px;background:#258045} input,select,[contenteditable] {display:block;margin:10px;width:180px;height:28px}a{display:block;margin:10px}</style>
</head><body><input id="field" value="initial"><div id="editor" contenteditable="true">synthetic editable</div>
<select id="choices"><option value="first">first</option><optgroup label="group"><option disabled value="disabled">disabled</option><option value="third" title="synthetic tooltip">third</option></optgroup></select>
<a id="link" href="synthetic://fixture/two">synthetic link</a>
<script>window.events=[];document.addEventListener('keydown',e=>events.push(e.key));window.pageSawStart=window.injectedStart??null;</script>
</body></html>"#;
