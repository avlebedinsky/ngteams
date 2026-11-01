//! A real disposable child owned by the unchanged browser shutdown policy.
use super::*;

impl Browser {
    /// Owns a test stand-in in exactly the same process group/lifecycle as a browser. It has no debugging endpoint.
    #[cfg(test)]
    pub(crate) fn synthetic_process(child: Child) -> Self {
        let group = Group(child.id().and_then(|pid| Pid::from_raw(pid.cast_signed())));
        Self {
            child,
            group,
            endpoint: DevToolsEndpoint {
                port: 1,
                path: "/devtools/browser/synthetic".into(),
            },
        }
    }
}
