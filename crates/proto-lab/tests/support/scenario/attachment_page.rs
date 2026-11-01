//! Attachment editor decisions over the shared chat peer and real forced-deny CDP path.

use serde_json::{Value, json};

use crate::scenario::ab_chat_full::tests::{EventsOut, broadcast, page_with as chat_page_with};
use crate::scenario::test_page::{Page, evaluated};

#[derive(Clone, Copy)]
pub(in crate::scenario) enum Kind {
    File,
    Image,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::scenario) enum Upload {
    Denied,
    Missing,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::scenario) enum Unsent {
    Absent,
    Delete,
    Stays,
    Broken,
}

struct Editor {
    kind: Kind,
    taken: &'static str,
    upload: Upload,
    unsent: Unsent,
    attached: bool,
    removed: bool,
    text: String,
    serial: usize,
}

impl Editor {
    fn denied(&mut self, relay: &EventsOut, message: bool) {
        self.serial += 1;
        let url = if message {
            "https://teams.microsoft.com/api/chatsvc/amer/v1/users/ME/conversations/19:synthetic-ab@thread.v2/messages"
        } else {
            match self.kind {
                Kind::File => "https://synthetic.sharepoint.com/drive/synthetic-upload",
                Kind::Image => "https://us-api.asm.skype.com/v1/objects/synthetic-object/content/imgpsh",
            }
        };
        let id = format!("attachment-{}", self.serial);
        let request = json!({"method":"POST", "url":url, "headers":{}, "postData":self.text});
        broadcast(
            relay,
            "Network.requestWillBeSent",
            json!({"requestId":format!("synthetic-{id}"),"request":request}),
        );
        broadcast(
            relay,
            "Fetch.requestPaused",
            json!({"requestId":format!("paused-{id}"),"networkId":format!("synthetic-{id}"),"resourceType":"XHR","request":request}),
        );
    }

    fn answer(&mut self, command: &Value, relay: &EventsOut) -> Option<Result<Value, String>> {
        match command["method"].as_str().unwrap() {
            "Runtime.evaluate" => self.evaluate(command["params"]["expression"].as_str().unwrap(), relay),
            "Input.insertText" => {
                self.text = command["params"]["text"].as_str().unwrap().to_owned();
                None
            }
            "Input.dispatchKeyEvent" if self.text.contains("caption") && command["params"]["key"] == "Enter" => {
                if command["params"]["type"] == "keyDown"
                    && matches!(self.kind, Kind::File)
                    && self.upload == Upload::Denied
                {
                    self.denied(relay, true);
                }
                Some(Ok(json!({})))
            }
            "Page.navigate" if self.attached && self.upload == Upload::Denied => {
                self.denied(relay, false);
                None
            }
            _ => None,
        }
    }

    fn evaluate(&mut self, expression: &str, relay: &EventsOut) -> Option<Result<Value, String>> {
        if expression.contains("editor.dispatchEvent(new ClipboardEvent") {
            if self.taken == "error" {
                return Some(Err("synthetic attachment editor failure".to_owned()));
            }
            if self.taken == "gone" {
                return Some(Ok(evaluated(json!(false))));
            }
            let how = if expression.contains("if (\"paste\" ===") {
                "paste"
            } else {
                "drop"
            };
            self.attached = self.taken == how;
            if self.attached && self.upload == Upload::Denied {
                self.denied(relay, false);
            }
            return Some(Ok(evaluated(json!(true))));
        }
        if expression.contains("HTMLInputElement.prototype.click") {
            return Some(Ok(evaluated(json!(true))));
        }
        if expression == crate::scenario::ab_file::CLICK_ATTACH {
            return Some(Ok(evaluated(if self.taken == "picker" {
                json!("Attach files")
            } else {
                Value::Null
            })));
        }
        if expression == crate::scenario::ab_file::PICK_UPLOAD {
            self.attached = self.taken == "picker";
            if self.attached && self.upload == Upload::Denied {
                self.denied(relay, false);
            }
            return Some(Ok(evaluated(json!(self.attached))));
        }
        if expression == "window.__labFileHanded ?? 0" {
            return Some(Ok(evaluated(json!(1))));
        }
        if expression.contains("e.tagName !== 'SCRIPT'") || expression == crate::scenario::ab_image::compose_has_image()
        {
            return Some(Ok(evaluated(json!(self.attached))));
        }
        if expression.contains("const stem =") || expression.contains("img[src^=\"blob:\"]") {
            if self.unsent == Unsent::Broken {
                return Some(Err("synthetic unsent editor failure".to_owned()));
            }
            let found = self.unsent != Unsent::Absent && (!self.removed || self.unsent == Unsent::Stays);
            return Some(Ok(evaluated(json!(if expression.starts_with('!') {
                !found
            } else {
                found
            }))));
        }
        if expression.contains("message-hover-action-delete") && expression.contains("e.click()") {
            self.removed = true;
            return Some(Ok(evaluated(json!(true))));
        }
        if expression == crate::scenario::ab_file::COMPOSE_CONTROLS {
            return Some(Ok(evaluated(json!(["Attach files"]))));
        }
        if expression == crate::scenario::ab_file::PAGE_NOTICES {
            return Some(Ok(evaluated(json!(["Synthetic storage unavailable"]))));
        }
        None
    }
}

pub(in crate::scenario) async fn page(kind: Kind, taken: &'static str, upload: Upload, unsent: Unsent) -> Page {
    page_with(kind, taken, upload, unsent, |_, _| None).await
}

pub(in crate::scenario) async fn page_with(
    kind: Kind,
    taken: &'static str,
    upload: Upload,
    unsent: Unsent,
    mut hook: impl FnMut(&Value, &EventsOut) -> Option<Result<Value, String>> + Send + 'static,
) -> Page {
    let mut editor = Editor {
        kind,
        taken,
        upload,
        unsent,
        attached: false,
        removed: false,
        text: String::new(),
        serial: 0,
    };
    chat_page_with(false, false, move |command, relay| {
        hook(command, relay).or_else(|| editor.answer(command, relay))
    })
    .await
}
