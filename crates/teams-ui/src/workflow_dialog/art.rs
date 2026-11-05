//! The app pictures Teams' Workflows shows, taken from Microsoft's own (`assets/apps/`, scaled down to 64 px): the
//! Workflows app's icon in the dialog's and the app page's header (the app catalog's large image) and each
//! connector's icon on the template cards, the card editor's chips and the Templates tab's Filter (Power Automate's
//! connector icons, by API name).
use std::sync::{Arc, OnceLock};

use gpui::{Image, ImageFormat};

fn png(cell: &'static OnceLock<Arc<Image>>, bytes: &'static [u8]) -> Arc<Image> {
    Arc::clone(cell.get_or_init(|| Arc::new(Image::from_bytes(ImageFormat::Png, bytes.to_vec()))))
}

/// The Workflows app's icon.
#[must_use]
pub fn workflows() -> Arc<Image> {
    static IMAGE: OnceLock<Arc<Image>> = OnceLock::new();
    png(&IMAGE, include_bytes!("../../assets/apps/workflows.png"))
}

/// Microsoft Teams' connector icon.
pub(super) fn teams() -> Arc<Image> {
    static IMAGE: OnceLock<Arc<Image>> = OnceLock::new();
    png(&IMAGE, include_bytes!("../../assets/apps/teams.png"))
}

/// One icon, read once.
macro_rules! icon {
    ($file:literal) => {{
        static IMAGE: OnceLock<Arc<Image>> = OnceLock::new();
        png(&IMAGE, include_bytes!(concat!("../../assets/apps/", $file, ".png")))
    }};
}

/// A connector's icon by its API name (`shared_planner`), Microsoft Teams' for one without its own.
pub(super) fn connector(api: &str) -> Arc<Image> {
    match api {
        "shared_planner" => icon!("planner"),
        "shared_office365users" => icon!("office365users"),
        "shared_sharepointonline" => icon!("sharepointonline"),
        "shared_office365" => icon!("office365"),
        "shared_rss" => icon!("rss"),
        "shared_m365copilotv2" => icon!("m365copilotv2"),
        "shared_visualstudioteamservices" => icon!("visualstudioteamservices"),
        "shared_office365groups" => icon!("office365groups"),
        "shared_pagerduty" => icon!("pagerduty"),
        "shared_service-now" => icon!("service-now"),
        "shared_conversionservice" => icon!("conversionservice"),
        "shared_onenote" => icon!("onenote"),
        "shared_github" => icon!("github"),
        "shared_onedriveforbusiness" => icon!("onedriveforbusiness"),
        "shared_jira" => icon!("jira"),
        "shared_approvals" => icon!("approvals"),
        _ => teams(),
    }
}

/// Teams' picture of starting a workflow from a message (a message, its hover bar and its menu), as the Workflows
/// app draws it on a message template's card and on such a workflow's Overview (its own `message-action.svg`, drawn
/// at twice the card's size).
pub(super) fn message_action() -> Arc<Image> {
    static IMAGE: OnceLock<Arc<Image>> = OnceLock::new();
    png(&IMAGE, include_bytes!("../../assets/apps/message-action.png"))
}
