//! What the engine is started with: where its frames go ([`FrameMode`]) and the URI schemes its pages may load
//! ([`HostConfig`]).
//!
//! The dmabuf target comes from the consumer (the GPUI frontend's wgpu adapter): `WebKit` has to render on the GPU that
//! imports its buffers, since a dmabuf made on another GPU fails to import (`docs/spikes/20261014-wpe-dmabuf.md`,
//! quirk 1), and offer only formats and modifiers that device imports.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// `DRM_FORMAT_MOD_LINEAR`: plain row-major memory, which every device that imports dmabufs at all imports.
pub const MODIFIER_LINEAR: u64 = 0;
/// `DRM_FORMAT_MOD_INVALID`: "the modifier is implicit", the driver's own choice.
pub const MODIFIER_INVALID: u64 = 0x00ff_ffff_ffff_ffff;

/// The DRM fourcc of `name`, four ASCII characters (`drm_fourcc.h`'s `fourcc_code`); `XR24` is XRGB8888.
#[must_use]
pub const fn fourcc(name: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*name)
}

/// A format the consumer imports, with every modifier it imports it with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BufferFormat {
    /// The DRM fourcc ([`fourcc`]).
    pub fourcc: u32,
    /// Best first: `WebKit` takes the first one its allocator supports.
    pub modifiers: Vec<u64>,
}

/// The device `WebKit` renders on and what it may hand over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DmaBufTarget {
    /// The DRM render node of the consumer's GPU (`/dev/dri/renderD128`).
    pub render_node: PathBuf,
    /// The formats it imports, preferred first.
    pub formats: Vec<BufferFormat>,
}

/// How frames reach the consumer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FrameMode {
    /// Zero-copy GPU buffers on the consumer's device: the path for everything, video included.
    DmaBuf(DmaBufTarget),
    /// Pixels in shared memory, copied out per damaged rectangle: a last-resort correctness fallback for documents
    /// when dmabuf import fails on a driver, never a performance path (Task 2: 80–115 % CPU on 1080p video, 2.5 fps on
    /// `YouTube`). `WebKit` renders on the CPU in this mode.
    Shm,
}

/// What the engine starts with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostConfig {
    pub frames: FrameMode,
    /// URI schemes served by the consumer ([`crate::Event::SchemeRequest`]), registered before any page exists.
    pub schemes: Vec<String>,
    /// Web process extensions every web process of the engine loads, set before any page exists.
    pub extensions: Option<WebProcessExtensions>,
}

/// Modules `WebKit` loads into each web process it starts (`webkit_web_context_set_web_process_extensions_directory`),
/// each initialized with `data`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebProcessExtensions {
    /// The directory every module of which is loaded (put only the modules meant there in it).
    pub directory: PathBuf,
    /// Handed to each module's `webkit_web_process_extension_initialize_with_user_data` as a `GVariant` string.
    pub data: String,
    /// Directories the web processes' sandbox shows (read-only), the module's own and what it reaches among them.
    pub sandbox_paths: Vec<PathBuf>,
}

/// Why a [`HostConfig`] cannot start an engine.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("not a DRM render node: {0} (expected /dev/dri/renderD<N>)")]
    NotARenderNode(PathBuf),
    #[error("no dmabuf format to offer: the consumer imports none")]
    NoFormats,
    #[error("dmabuf format {0:#010x} lists no modifier")]
    NoModifiers(u32),
    #[error("not a URI scheme: {0:?}")]
    InvalidScheme(String),
    #[error("the {0} scheme is the web engine's own")]
    ReservedScheme(String),
    #[error("the {0} scheme is registered twice")]
    DuplicateScheme(String),
}

/// Schemes `WebKit` handles itself; a page's own scheme must not shadow them.
const RESERVED_SCHEMES: [&str; 11] = [
    "about",
    "blob",
    "data",
    "file",
    "ftp",
    "http",
    "https",
    "javascript",
    "webkit",
    "ws",
    "wss",
];

impl HostConfig {
    /// Checks everything that can be checked before the engine starts.
    ///
    /// # Errors
    ///
    /// The first problem found: a render node that is not one, no formats or a format without modifiers, a scheme
    /// that is malformed, reserved or listed twice.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if let FrameMode::DmaBuf(target) = &self.frames {
            target.validate()?;
        }
        let mut seen: Vec<String> = Vec::new();
        for scheme in &self.schemes {
            if !is_scheme(scheme) {
                return Err(ConfigError::InvalidScheme(scheme.clone()));
            }
            let lower = scheme.to_ascii_lowercase();
            if RESERVED_SCHEMES.contains(&lower.as_str()) {
                return Err(ConfigError::ReservedScheme(lower));
            }
            if seen.contains(&lower) {
                return Err(ConfigError::DuplicateScheme(lower));
            }
            seen.push(lower);
        }
        Ok(())
    }
}

impl DmaBufTarget {
    fn validate(&self) -> Result<(), ConfigError> {
        if !is_render_node(&self.render_node) {
            return Err(ConfigError::NotARenderNode(self.render_node.clone()));
        }
        if self.formats.is_empty() {
            return Err(ConfigError::NoFormats);
        }
        if let Some(format) = self.formats.iter().find(|format| format.modifiers.is_empty()) {
            return Err(ConfigError::NoModifiers(format.fourcc));
        }
        Ok(())
    }
}

/// `/dev/dri/renderD<N>`: a render node by name (the device itself is opened by the engine, which reports a missing
/// one when it starts).
fn is_render_node(path: &Path) -> bool {
    path.parent() == Some(Path::new("/dev/dri"))
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix("renderD"))
            .is_some_and(|number| !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit()))
}

/// RFC 3986: a letter, then letters, digits, `+`, `-` or `.`.
fn is_scheme(scheme: &str) -> bool {
    let mut chars = scheme.chars();
    chars.next().is_some_and(|first| first.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dmabuf(render_node: &str, formats: Vec<BufferFormat>) -> HostConfig {
        HostConfig {
            frames: FrameMode::DmaBuf(DmaBufTarget {
                render_node: PathBuf::from(render_node),
                formats,
            }),
            schemes: Vec::new(),
            extensions: None,
        }
    }

    fn xrgb_linear() -> BufferFormat {
        BufferFormat {
            fourcc: fourcc(b"XR24"),
            modifiers: vec![MODIFIER_LINEAR],
        }
    }

    #[test]
    fn fourcc_codes_match_drm_fourcc_h() {
        assert_eq!(fourcc(b"XR24"), 0x3432_5258, "DRM_FORMAT_XRGB8888");
        assert_eq!(fourcc(b"AR24"), 0x3432_5241, "DRM_FORMAT_ARGB8888");
    }

    #[test]
    fn a_dmabuf_target_needs_a_render_node_and_formats_with_modifiers() {
        assert_eq!(dmabuf("/dev/dri/renderD128", vec![xrgb_linear()]).validate(), Ok(()));
        for node in [
            "/dev/dri/card0",
            "/dev/dri/renderD",
            "/dev/dri/renderDx",
            "/tmp/renderD128",
            "renderD128",
        ] {
            assert_eq!(
                dmabuf(node, vec![xrgb_linear()]).validate(),
                Err(ConfigError::NotARenderNode(PathBuf::from(node))),
                "{node}"
            );
        }
        assert_eq!(
            dmabuf("/dev/dri/renderD129", Vec::new()).validate(),
            Err(ConfigError::NoFormats)
        );
        let bare = BufferFormat {
            fourcc: fourcc(b"AR24"),
            modifiers: Vec::new(),
        };
        assert_eq!(
            dmabuf("/dev/dri/renderD129", vec![xrgb_linear(), bare]).validate(),
            Err(ConfigError::NoModifiers(fourcc(b"AR24")))
        );
    }

    #[test]
    fn schemes_must_be_well_formed_unique_and_not_the_engine_s() {
        let config = |schemes: &[&str]| HostConfig {
            frames: FrameMode::Shm,
            schemes: schemes.iter().map(|scheme| (*scheme).to_owned()).collect(),
            extensions: None,
        };
        assert_eq!(config(&["ngterm-file", "x.y+z"]).validate(), Ok(()));
        assert_eq!(
            config(&["9lives"]).validate(),
            Err(ConfigError::InvalidScheme("9lives".to_owned()))
        );
        assert_eq!(config(&[""]).validate(), Err(ConfigError::InvalidScheme(String::new())));
        assert_eq!(
            config(&["a b"]).validate(),
            Err(ConfigError::InvalidScheme("a b".to_owned()))
        );
        assert_eq!(
            config(&["ftp"]).validate(),
            Err(ConfigError::ReservedScheme("ftp".to_owned())),
            "WebKit refuses to register its special schemes"
        );
        assert_eq!(
            config(&["HTTPS"]).validate(),
            Err(ConfigError::ReservedScheme("https".to_owned()))
        );
        assert_eq!(
            config(&["page", "Page"]).validate(),
            Err(ConfigError::DuplicateScheme("page".to_owned()))
        );
    }
}
