//! The Teams web bundles: unpacked from the Windows MSIX into the lab's `bundles/<version>/` (outside the repo) and
//! indexed for URL templates, hosts, HTTP methods and routes. Static analysis only: nothing from a bundle is ever
//! executed.

pub mod import;
pub mod index;
pub mod literals;
