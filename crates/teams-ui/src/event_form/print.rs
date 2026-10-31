//! The scheduling form's Print, as Teams prints an event: a page of the event's details (its title, when it is and
//! how it repeats, where, the Teams meeting, who is invited, how it shows the account's time, its reminder and
//! categories, and its text) handed to the desktop's browser, which opens its print dialog on it.
//!
//! The page is written to a file of the account's own (`$XDG_RUNTIME_DIR`, else the temporary directory; readable by
//! the user alone) and opened as a `file://` link. Its text is escaped and its body is the form's own rich text
//! written again as the editor writes a message (`teams-proto`'s message text: no script, no style, links only
//! `http`, `https` and `mailto`), never the HTML an invitation came with; the page's policy lets nothing load.

use std::io::Write as _;
use std::path::PathBuf;

/// What the printed page shows, each line as the form shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Printable {
    pub title: String,
    pub when: String,
    /// How it repeats ("Occurs every Tuesday starting 10/27/2026"), for a series.
    pub repeats: Option<String>,
    pub location: String,
    pub online: bool,
    pub required: Vec<String>,
    pub optional: Vec<String>,
    pub show_as: String,
    pub reminder: String,
    pub categories: Vec<String>,
    pub private: bool,
    /// The text as HTML, written from the form's rich text (see the module docs); `None` for none.
    pub body_html: Option<String>,
    /// The text as plain lines, when its HTML couldn't be written.
    pub body_text: String,
}

/// `text` escaped for HTML.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// The page that prints `event`: its details as a table, its text below, and a script that opens the print dialog.
#[must_use]
pub fn page(event: &Printable) -> String {
    let title = if event.title.trim().is_empty() {
        "Untitled event".to_owned()
    } else {
        event.title.trim().to_owned()
    };
    let mut rows = vec![("When", event.when.clone())];
    rows.extend(event.repeats.clone().map(|repeats| ("Repeats", repeats)));
    if !event.location.trim().is_empty() {
        rows.push(("Where", event.location.trim().to_owned()));
    }
    if event.online {
        rows.push(("Meeting", "Microsoft Teams meeting".to_owned()));
    }
    if !event.required.is_empty() {
        rows.push(("Required", event.required.join("; ")));
    }
    if !event.optional.is_empty() {
        rows.push(("Optional", event.optional.join("; ")));
    }
    rows.push(("Show as", event.show_as.clone()));
    rows.push(("Reminder", event.reminder.clone()));
    if !event.categories.is_empty() {
        rows.push(("Categories", event.categories.join(", ")));
    }
    if event.private {
        rows.push(("Private", "Yes".to_owned()));
    }
    let table: String = rows
        .iter()
        .map(|(name, value)| format!("<tr><th>{name}</th><td>{}</td></tr>", escape(value)))
        .collect();
    let body = match &event.body_html {
        Some(html) => html.clone(),
        None => event
            .body_text
            .lines()
            .map(|line| format!("<p>{}</p>", escape(line)))
            .collect(),
    };
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\">\
         <meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'; \
         script-src 'unsafe-inline'\">\
         <title>{title}</title><style>{STYLE}</style></head><body>\
         <h1>{title}</h1><table>{table}</table><hr><div class=\"text\">{body}</div>\
         <script>window.addEventListener('load', () => window.print());</script></body></html>",
        title = escape(&title),
    )
}

/// The printed page's look: plain, black on white, as a printout.
const STYLE: &str = "body{font-family:'Segoe UI',system-ui,sans-serif;color:#000;background:#fff;margin:32px;\
                     font-size:14px}h1{font-size:22px;margin:0 0 16px}th{text-align:left;vertical-align:top;\
                     padding:4px 16px 4px 0;color:#444;font-weight:600;white-space:nowrap}td{padding:4px 0}\
                     hr{border:0;border-top:1px solid #ccc;margin:16px 0}.text p{margin:0 0 6px}";

/// Where the page is written: the user's runtime directory, else the temporary one.
fn page_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_dir())
        .unwrap_or_else(std::env::temp_dir);
    dir.join(format!("ngteams-event-print-{}.html", std::process::id()))
}

/// Writes `page` where the desktop's browser can open it (readable by the user alone) and gives its `file://` link.
///
/// # Errors
/// What the file system said, in plain words.
pub fn write(page: &str) -> Result<String, String> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let path = page_path();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .map_err(|error| format!("the page to print couldn't be written: {error}"))?;
    file.write_all(page.as_bytes())
        .map_err(|error| format!("the page to print couldn't be written: {error}"))?;
    Ok(format!("file://{}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_escapes_what_it_shows_and_prints_itself() {
        let page = page(&Printable {
            title: "Plan <b>this</b>".to_owned(),
            when: "Tue 10/27/2026 3:00 PM - 3:30 PM".to_owned(),
            repeats: Some("Occurs every Tuesday starting 10/27/2026".to_owned()),
            required: vec!["Bot B".to_owned()],
            show_as: "Busy".to_owned(),
            reminder: "15 minutes before".to_owned(),
            body_text: "a < b".to_owned(),
            ..Printable::default()
        });
        assert!(page.contains("<h1>Plan &lt;b&gt;this&lt;/b&gt;</h1>"));
        assert!(page.contains("<th>Repeats</th><td>Occurs every Tuesday starting 10/27/2026</td>"));
        assert!(page.contains("<p>a &lt; b</p>"));
        assert!(page.contains("window.print()"));
        assert!(!page.contains("<th>Where</th>"));
    }
}
