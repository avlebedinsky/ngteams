//! The app pictures Teams' Workflows dialog shows, taken from Microsoft's own (`assets/apps/`, scaled down): the
//! Workflows app's icon in the dialog's header (the app catalog's large image) and each connector's icon on the card
//! editor's chips (Power Automate's connector icons: `teams`, `planner`, `office365users`, `sharepointonline`,
//! `office365`).
use std::sync::{Arc, OnceLock};

use gpui::{Image, ImageFormat};

fn png(cell: &'static OnceLock<Arc<Image>>, bytes: &'static [u8]) -> Arc<Image> {
    Arc::clone(cell.get_or_init(|| Arc::new(Image::from_bytes(ImageFormat::Png, bytes.to_vec()))))
}

/// The Workflows app's icon.
pub(super) fn workflows() -> Arc<Image> {
    static IMAGE: OnceLock<Arc<Image>> = OnceLock::new();
    png(&IMAGE, include_bytes!("../../assets/apps/workflows.png"))
}

/// Microsoft Teams' connector icon.
pub(super) fn teams() -> Arc<Image> {
    static IMAGE: OnceLock<Arc<Image>> = OnceLock::new();
    png(&IMAGE, include_bytes!("../../assets/apps/teams.png"))
}

/// A connector's icon by its API name (`shared_planner`), Microsoft Teams' for one without its own.
pub(super) fn connector(api: &str) -> Arc<Image> {
    static PLANNER: OnceLock<Arc<Image>> = OnceLock::new();
    static USERS: OnceLock<Arc<Image>> = OnceLock::new();
    static SHAREPOINT: OnceLock<Arc<Image>> = OnceLock::new();
    static OUTLOOK: OnceLock<Arc<Image>> = OnceLock::new();
    match api {
        "shared_planner" => png(&PLANNER, include_bytes!("../../assets/apps/planner.png")),
        "shared_office365users" => png(&USERS, include_bytes!("../../assets/apps/office365users.png")),
        "shared_sharepointonline" => png(&SHAREPOINT, include_bytes!("../../assets/apps/sharepointonline.png")),
        "shared_office365" => png(&OUTLOOK, include_bytes!("../../assets/apps/office365.png")),
        _ => teams(),
    }
}
