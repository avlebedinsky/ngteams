//! The app pictures Teams' Workflows dialog shows, taken from Microsoft's own (`assets/apps/`, scaled down): the
//! Workflows app's icon in the dialog's header (the app catalog's large image) and Microsoft Teams' connector icon on
//! the card editor's "Microsoft Teams" chip (Power Automate's `teams` connector icon).
use std::sync::{Arc, OnceLock};

use gpui::{Image, ImageFormat};

/// The Workflows app's icon.
pub(super) fn workflows() -> Arc<Image> {
    static IMAGE: OnceLock<Arc<Image>> = OnceLock::new();
    Arc::clone(IMAGE.get_or_init(|| {
        Arc::new(Image::from_bytes(
            ImageFormat::Png,
            include_bytes!("../../assets/apps/workflows.png").to_vec(),
        ))
    }))
}

/// Microsoft Teams' connector icon.
pub(super) fn teams() -> Arc<Image> {
    static IMAGE: OnceLock<Arc<Image>> = OnceLock::new();
    Arc::clone(IMAGE.get_or_init(|| {
        Arc::new(Image::from_bytes(
            ImageFormat::Png,
            include_bytes!("../../assets/apps/teams.png").to_vec(),
        ))
    }))
}
