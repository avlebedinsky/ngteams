//! Link previews under `/ams/urlp/v1/url/` (`docs/protocol/chat.md`, "Link previews"): what a page says about
//! itself, and a thumbnail of its picture, both read by the server ([`super::fetch`]) so a reader's client never
//! reaches the page's site. The thumbnail is the read the client makes for a message's `previewurl`.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, X_CONTENT_TYPE_OPTIONS};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use scraper::{Html, Selector};
use serde_json::{Value, json};
use url::Url;

use super::fetch::FetchError;
use super::{Error, IC3, account, thumbs};
use crate::app::App;

/// The longest side of a preview's thumbnail: twice what a card draws.
const THUMBNAIL_SIDE: u32 = 640;
/// The longest title and description answered, in characters, and the longest picture link, in bytes.
const MAX_TITLE: usize = 300;
const MAX_DESCRIPTION: usize = 1000;
const MAX_LINK: usize = 2048;

pub(super) fn router() -> Router<Arc<App>> {
    Router::new()
        .route("/ams/urlp/v1/url/info", get(info))
        .route("/ams/urlp/v1/url/image/Thumbnail", get(thumbnail))
}

impl From<FetchError> for Error {
    fn from(error: FetchError) -> Self {
        match error {
            FetchError::Url => Self::Invalid("The link can't be previewed."),
            FetchError::NotPublic => Self::Forbidden,
            FetchError::Busy => Self::Busy,
            FetchError::TooLarge => Self::Unsupported,
            FetchError::Failed => Self::Upstream,
        }
    }
}

fn asked(query: &HashMap<String, String>) -> Result<&str, Error> {
    query
        .get("url")
        .map(String::as_str)
        .ok_or(Error::Invalid("The request names no link."))
}

/// What a page says about itself.
#[derive(Debug, Default, PartialEq, Eq)]
struct Page {
    title: Option<String>,
    description: Option<String>,
    picture: Option<String>,
    favicon: Option<String>,
}

/// Reads `html`'s Open Graph and plain metadata; its links are made absolute on `base`.
fn read_page(html: &str, base: &Url) -> Page {
    let document = Html::parse_document(html);
    let content = |selectors: &[&str]| {
        selectors.iter().find_map(|selector| {
            let selector = Selector::parse(selector).ok()?;
            let found = document.select(&selector).next()?;
            let text = match found.value().name() {
                "title" => found.text().collect::<String>(),
                "link" => found.value().attr("href")?.to_owned(),
                _ => found.value().attr("content")?.to_owned(),
            };
            let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
            (!text.is_empty()).then_some(text)
        })
    };
    let link = |found: Option<String>| {
        let url = base.join(&found?).ok()?;
        (matches!(url.scheme(), "http" | "https") && url.as_str().len() <= MAX_LINK).then(|| String::from(url))
    };
    let cut = |text: Option<String>, most: usize| text.map(|text| text.chars().take(most).collect::<String>());
    Page {
        title: cut(
            content(&[r#"meta[property="og:title"]"#, r#"meta[name="twitter:title"]"#, "title"]),
            MAX_TITLE,
        ),
        description: cut(
            content(&[r#"meta[property="og:description"]"#, r#"meta[name="description"]"#]),
            MAX_DESCRIPTION,
        ),
        picture: link(content(&[
            r#"meta[property="og:image"]"#,
            r#"meta[name="twitter:image"]"#,
        ])),
        favicon: link(content(&[r#"link[rel~="icon"]"#]).or_else(|| Some("/favicon.ico".to_owned()))),
    }
}

async fn info(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, Error> {
    account(&app, &headers, IC3).await?;
    let url = asked(&query)?;
    let fetched = app
        .fetcher
        .get(url, app.config.media.preview_max_page_bytes, true)
        .await?;
    let page = read_page(&String::from_utf8_lossy(&fetched.bytes), &fetched.url);
    let preview = page.picture.as_deref().and_then(|picture| {
        let mut link = Url::parse(&format!("{}/ams/urlp/v1/url/image/Thumbnail", app.config.origin())).ok()?;
        link.query_pairs_mut().append_pair("url", picture);
        Some(String::from(link))
    });
    Ok(Json(json!({
        "url": url, "status": 200, "title": page.title, "favicon": page.favicon,
        "preview": {
            "title": page.title, "description": page.description,
            "previewurl": preview.unwrap_or_default(),
        },
    })))
}

async fn thumbnail(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Response, Error> {
    account(&app, &headers, IC3).await?;
    let fetched = app
        .fetcher
        .get(asked(&query)?, app.config.media.preview_max_picture_bytes, false)
        .await?;
    let picture = thumbs::decoding(fetched.bytes.into(), |source| {
        thumbs::jpeg(&thumbs::decode(source)?, THUMBNAIL_SIDE)
    })
    .await
    .ok_or(Error::Unsupported)?;
    Ok((
        [
            (CONTENT_TYPE, "image/jpeg"),
            (CACHE_CONTROL, "private, max-age=86400"),
            (X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        picture.2,
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_is_read_by_its_open_graph_tags_then_its_plain_ones() {
        let base = Url::parse("https://site.example/a/page").unwrap();
        let page = read_page(
            r#"<title> Plain   title </title><meta name="description" content="Plain words">
               <meta name="twitter:image" content="//cdn.example/p.png"><link rel="shortcut icon" href="i.ico">"#,
            &base,
        );
        assert_eq!(page.title.as_deref(), Some("Plain title"));
        assert_eq!(page.description.as_deref(), Some("Plain words"));
        assert_eq!(page.picture.as_deref(), Some("https://cdn.example/p.png"));
        assert_eq!(page.favicon.as_deref(), Some("https://site.example/a/i.ico"));
        let empty = read_page(r#"<meta property="og:image" content="javascript:alert(1)">"#, &base);
        assert_eq!(
            empty,
            Page {
                favicon: Some("https://site.example/favicon.ico".to_owned()),
                ..Page::default()
            }
        );
        let long = read_page(&format!("<title>{}</title>", "t".repeat(900)), &base);
        assert_eq!(long.title.unwrap().len(), MAX_TITLE);
    }
}
