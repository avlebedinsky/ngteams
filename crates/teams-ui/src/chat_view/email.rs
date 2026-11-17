//! A channel post's subject, and a post that came by email, drawn as Teams' channel draws them
//! (`teams_proto::chat::EmailDetails`, `docs/protocol/channels.md`, "A post that came by email"):
//!
//! - **A subject** (`properties.subject`) is drawn bold over the post's text.
//! - **An email post** has the mail icon in place of the sender's avatar, "<sender> via email" beside its time, a
//!   "To:" line under them (at most [`teams_proto::chat::SHOWN_RECIPIENTS`] recipients, then "+N"), and its card
//!   ends with its reactions, "View original email" and, in the channel's list, "Reply in thread".
//! - **"View original email"** gets the original `.eml` file, as Teams' download does: a SharePoint file opens in the
//!   browser with `download=1` (the browser downloads it), an own server's file is saved through its account's engine
//!   ([`crate::files::OwnFile`]); a line under the row tells how far that download got, or why it failed.

use gpui::{
    AnyElement, App, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px, rgb,
};
use teams_proto::chat::EmailDetails;

use crate::forwarded::OnOpen;
use crate::icons::Glyph;
use crate::inline::PictureSource;
use crate::people::Face;
use crate::theme::Palette;

/// What the sender's name says on an email post.
pub const VIA_EMAIL: &str = "via email";

/// The row that gets the original email.
pub const VIEW_ORIGINAL_EMAIL: &str = "View original email";

/// The row that opens the post's thread.
pub const REPLY_IN_THREAD: &str = "Reply in thread";

/// The size of the mail icon in the avatar's place.
const MAIL_ICON: f32 = 28.0;

/// What an email post shows besides a post's text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmailPost {
    /// The "To:" line's recipients: the names (or addresses) shown, then "+N" for the others. `None` without any.
    pub to: Option<SharedString>,
    /// The original `.eml` file's link, when there is one to open.
    pub link: Option<String>,
}

impl EmailPost {
    #[must_use]
    pub fn of(details: &EmailDetails) -> Self {
        let mut to = details
            .shown_recipients()
            .iter()
            .map(|user| user.label())
            .collect::<Vec<_>>()
            .join(", ");
        let more = details.more_recipients();
        if more > 0 {
            if !to.is_empty() {
                to.push(' ');
            }
            to.push('+');
            to.push_str(&more.to_string());
        }
        Self {
            to: (!to.is_empty()).then(|| format!("To: {to}").into()),
            link: details.email_link.clone(),
        }
    }
}

/// The mail icon that stands in the sender's avatar's place.
pub(super) fn mail_avatar(id: &str, palette: Palette) -> AnyElement {
    let id = id.to_owned();
    div()
        .debug_selector(move || format!("email-icon-{id}"))
        .size(px(super::AVATAR))
        .flex()
        .items_center()
        .justify_center()
        .child(Glyph::MAIL.icon().size(px(MAIL_ICON)).text_color(rgb(palette.brand)))
        .into_any_element()
}

/// The "To:" line under an email post's name and time.
pub(super) fn to_line(id: &str, to: &SharedString, palette: Palette) -> AnyElement {
    let id = id.to_owned();
    div()
        .debug_selector(move || format!("email-to-{id}"))
        .pb_1()
        .text_xs()
        .text_color(rgb(palette.muted_foreground))
        .truncate()
        .child(to.clone())
        .into_any_element()
}

/// A post's subject, bold over its text.
pub(super) fn subject_line(id: &str, subject: &SharedString, palette: Palette) -> AnyElement {
    let id = id.to_owned();
    div()
        .debug_selector(move || format!("post-subject-{id}"))
        .pb_2()
        .text_lg()
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(rgb(palette.foreground))
        .child(subject.clone())
        .into_any_element()
}

/// One of the rows at the foot of an email post's card: a hairline over it, its glyph and its words.
fn card_row(selector: String, lead: AnyElement, words: &'static str, palette: Palette, open: OnOpen) -> AnyElement {
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .mx(px(-super::BUBBLE_INSET))
        .px(px(super::BUBBLE_INSET))
        .py_2()
        .flex()
        .items_center()
        .gap_2()
        .border_t_1()
        .border_color(rgb(palette.border))
        .cursor_pointer()
        .hover(move |row| row.bg(rgb(palette.hover)))
        .on_click(open)
        .child(lead)
        .child(
            div()
                .text_sm()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(rgb(palette.muted_foreground))
                .child(words),
        )
        .into_any_element()
}

/// "View original email", which gets the original email (`link`); `source` is the engine an own server's file is read
/// through. Under it, a line tells how far that download got, as under a file's card.
pub(super) fn original_row(
    id: &str,
    link: &str,
    source: Option<&PictureSource>,
    palette: Palette,
    cx: &App,
) -> AnyElement {
    let status = (!teams_proto::chat::is_file_link(link))
        .then(|| crate::files::download_line(link, &file_name(link), &format!("email-download-{id}"), palette, cx))
        .flatten();
    let (link, source) = (link.to_owned(), source.cloned());
    let open: OnOpen = Box::new(move |_, _, cx| {
        cx.stop_propagation();
        open_original(&link, source.as_ref(), cx);
    });
    let glyph = Glyph::MAIL
        .icon()
        .size(px(16.0))
        .text_color(rgb(palette.muted_foreground))
        .into_any_element();
    let row = card_row(
        format!("email-original-{id}"),
        glyph,
        VIEW_ORIGINAL_EMAIL,
        palette,
        open,
    );
    div().flex().flex_col().child(row).children(status).into_any_element()
}

/// "Reply in thread" with the account's face, which opens the post's thread (`open`).
pub(super) fn reply_row(id: &str, me: (&SharedString, &Face), palette: Palette, open: OnOpen) -> AnyElement {
    let (name, face) = me;
    let avatar = face
        .avatar(name, gpui_component::Size::XSmall)
        .size(px(20.0))
        .into_any_element();
    card_row(format!("email-reply-{id}"), avatar, REPLY_IN_THREAD, palette, open)
}

/// Gets the original email at `link`: a SharePoint file through the browser's download, an own server's through its
/// engine (`source`); nothing when an own server's file has no engine to read it.
pub fn open_original(link: &str, source: Option<&PictureSource>, cx: &mut App) {
    if teams_proto::chat::is_file_link(link) {
        if let Ok(mut url) = url::Url::parse(link) {
            url.query_pairs_mut().append_pair("download", "1");
            cx.open_url(url.as_str());
        }
        return;
    }
    if let Some(source) = source {
        crate::files::OwnFile::new(link.to_owned(), file_name(link), source.clone()).download(cx);
    }
}

/// The file name at the end of `link`'s path, decoded; "email.eml" when it has none.
fn file_name(link: &str) -> String {
    url::Url::parse(link)
        .ok()
        .and_then(|url| {
            let last = url.path_segments()?.next_back()?.to_owned();
            let decoded = percent_encoding::percent_decode_str(&last)
                .decode_utf8()
                .ok()?
                .into_owned();
            (!decoded.trim().is_empty()).then_some(decoded)
        })
        .unwrap_or_else(|| "email.eml".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_name_comes_from_the_link_s_path() {
        assert_eq!(
            file_name("https://own.example.com/sp/files/Made%20up%20alert.eml"),
            "Made up alert.eml"
        );
        assert_eq!(file_name("https://own.example.com/"), "email.eml");
        assert_eq!(file_name("not a link"), "email.eml");
    }
}
