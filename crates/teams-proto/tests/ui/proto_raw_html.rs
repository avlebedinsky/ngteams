// A message's text is a `MessageText`, which `teams-proto` renders from a typed rich-text value and the MRIs its
// mentions name: raw HTML can't be posted or edited in, and no `MessageText` can be made from it or assembled by hand,
// preserving the sanitized typed composition boundary.
use teams_proto::chat::MessageText;
use teams_proto::{Author, EditMessage, Original, SendMessage};

fn write(author: Author, original: Original) {
    let _ = SendMessage::new("19:lab-chat@thread.v2", author.clone(), "<p>raw</p>");
    let _ = EditMessage::new("19:lab-chat@thread.v2", "1", author, "<p>raw</p>".to_owned(), original);
    let _ = MessageText::from("<p>raw</p>");
    let _ = MessageText { html: "<p>raw</p>".to_owned() };
}

fn main() {}
