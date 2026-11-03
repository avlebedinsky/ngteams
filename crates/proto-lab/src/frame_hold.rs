//! A selective WebSocket capture hold, used only by the activity-report experiments.
//! Unmatched frames use the original send method synchronously; there is no permission handshake or audit.
use serde_json::{Value, json};

pub(crate) const BINDING: &str = "__labHeldFrame";
pub(crate) const RULES: &str = "__labFrameHolds";
#[derive(Clone, Debug)]
pub struct FrameHold {
    url: regex::Regex,
    text: String,
    label: Option<String>,
}
impl FrameHold {
    /// # Errors
    /// When the URL expression is invalid.
    pub fn new(pattern: &str) -> Result<Self, regex::Error> {
        Ok(Self {
            url: regex::Regex::new(pattern)?,
            text: String::new(),
            label: None,
        })
    }
    #[must_use]
    pub fn with_text_containing(mut self, text: &str) -> Self {
        self.text = text.into();
        self
    }
    #[must_use]
    pub fn labelled(mut self, label: &str) -> Self {
        self.label = Some(label.into());
        self
    }
    #[must_use]
    pub fn matches(&self, url: &str, text: &str) -> bool {
        self.url.is_match(url) && text.contains(&self.text)
    }
    pub(crate) fn value(&self) -> Value {
        json!({"url":self.url.as_str(),"text":self.text,"label":self.label})
    }
}
pub(crate) fn rules(rules: &[FrameHold]) -> String {
    format!(
        "globalThis.{RULES}={};",
        json!(rules.iter().map(FrameHold::value).collect::<Vec<_>>())
    )
}
pub(crate) fn script(holds: &[FrameHold]) -> String {
    format!(
        r"{} (() => {{
      if (!globalThis.WebSocket || globalThis.__labFrameHoldInstalled) return;
      globalThis.__labFrameHoldInstalled=true;
      const send=WebSocket.prototype.send;
      WebSocket.prototype.send=function(data) {{
        const held=typeof data==='string' && (globalThis.{} ?? []).find(r=>new RegExp(r.url).test(this.url) && data.includes(r.text));
        if (held) {{ globalThis.{}(JSON.stringify({{url:this.url,data,label:held.label}})); return; }}
        return Reflect.apply(send,this,[data]);
      }};
    }})();",
        rules(holds),
        RULES,
        BINDING
    )
}
