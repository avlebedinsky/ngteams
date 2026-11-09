//! Shared command mappings for the socket client and the engine's local/worker test host.
//!
//! Both hosts supply `send`; evaluation and shutdown depend on their transport and stay with each host.

/// Implements the common page and session commands on a host with a `send(Command)` method and an id allocator.
#[doc(hidden)]
#[macro_export]
macro_rules! host_commands {
    ($host:ty, $next_id:path) => {
        impl $host {
            fn view(&self, view: $crate::ViewId, command: $crate::host::ViewCommand) {
                self.send($crate::host::Command::View(view, command));
            }

            /// Creates a browser store.
            #[must_use]
            pub fn create_session(&self, spec: $crate::SessionSpec) -> $crate::SessionId {
                let id = $crate::SessionId::from_raw($next_id());
                self.send($crate::host::Command::CreateSession(id, spec));
                id
            }

            /// Drops the host's hold on a store; pages built on it keep it until they close.
            pub fn drop_session(&self, session: $crate::SessionId) {
                self.send($crate::host::Command::DropSession(session));
            }

            /// Removes everything a store holds; [`Event::SessionCleared`] reports the outcome.
            pub fn clear_session(&self, session: $crate::SessionId) {
                self.send($crate::host::Command::ClearSession(session));
            }

            /// Creates a page, mapped and visible, loading nothing yet.
            #[must_use]
            pub fn create_view(&self, spec: $crate::ViewSpec) -> $crate::ViewId {
                let id = $crate::ViewId::from_raw($next_id());
                self.send($crate::host::Command::CreateView(id, Box::new(spec)));
                id
            }

            /// Closes a page: its load stops, the questions it left open are answered, its buffers go.
            pub fn close_view(&self, view: $crate::ViewId) {
                self.send($crate::host::Command::CloseView(view));
            }

            pub fn load(&self, view: $crate::ViewId, load: $crate::Load) {
                self.view(view, $crate::host::ViewCommand::Load(load));
            }

            pub fn reload(&self, view: $crate::ViewId) {
                self.view(view, $crate::host::ViewCommand::Reload);
            }

            pub fn stop(&self, view: $crate::ViewId) {
                self.view(view, $crate::host::ViewCommand::Stop);
            }

            pub fn go_back(&self, view: $crate::ViewId) {
                self.view(view, $crate::host::ViewCommand::GoBack);
            }

            pub fn go_forward(&self, view: $crate::ViewId) {
                self.view(view, $crate::host::ViewCommand::GoForward);
            }

            pub fn set_zoom(&self, view: $crate::ViewId, zoom: f64) {
                self.view(view, $crate::host::ViewCommand::Zoom(zoom));
            }

            /// Replaces the page's user scripts; they run from its next load on.
            pub fn set_scripts(&self, view: $crate::ViewId, scripts: Vec<$crate::UserScript>) {
                self.view(view, $crate::host::ViewCommand::Scripts(scripts));
            }

            pub fn set_background(&self, view: $crate::ViewId, background: Option<$crate::Rgba>) {
                self.view(view, $crate::host::ViewCommand::Background(background));
            }

            pub fn set_geometry(&self, view: $crate::ViewId, geometry: $crate::Geometry) {
                self.view(view, $crate::host::ViewCommand::Geometry(geometry));
            }

            /// A hidden page renders nothing.
            pub fn set_visible(&self, view: $crate::ViewId, visible: bool) {
                self.view(view, $crate::host::ViewCommand::Visible(visible));
            }

            /// Gives the page the keyboard focus, or takes it away.
            pub fn set_focused(&self, view: $crate::ViewId, focused: bool) {
                self.view(view, $crate::host::ViewCommand::Focused(focused));
            }

            pub fn input(&self, view: $crate::ViewId, input: $crate::Input) {
                self.view(view, $crate::host::ViewCommand::Input(input));
            }

            /// The frame in `buffer` is on screen: the page may render the next one, and the buffer shown before it
            /// goes
            /// back (crate `frames`).
            pub fn frame_presented(&self, view: $crate::ViewId, buffer: $crate::BufferId) {
                self.view(view, $crate::host::ViewCommand::Presented(buffer));
            }

            /// Switches a running page to SHM frames, for good: the consumer could not import its dmabufs (a format
            /// outside
            /// what it offered, a failed import). The page's web process is replaced and the page reloads what it shows
            /// (its history stays; the document's script state does not, and a page loaded from text should be loaded
            /// again); the frames that follow arrive as [`crate::FrameContent::Shm`], and the dmabuf shown last is
            /// released
            /// once an SHM frame replaces it. The engine's own mode cannot change after the start (`WebKit` hands its
            /// web
            /// processes the render node once per process, and a web process picks its buffers when it builds a page's
            /// surface), so this is the per-page fall-back. A page in an SHM engine is left alone.
            pub fn fall_back_to_shm(&self, view: $crate::ViewId) {
                self.view(view, $crate::host::ViewCommand::FallBackToShm);
            }

            /// Answers a [`crate::PolicyRequest`]: go on with it, or ignore it.
            pub fn decide_policy(&self, token: $crate::PolicyToken, allow: bool) {
                self.send($crate::host::Command::Policy(token, allow));
            }

            /// Answers a [`crate::ScriptMessage`]: the page's promise resolves with the JSON value, or rejects with the
            /// message.
            pub fn reply_script_message(&self, token: $crate::ReplyToken, reply: Result<String, String>) {
                self.send($crate::host::Command::Reply(token, reply));
            }

            /// Answers a [`crate::SchemeRequest`].
            pub fn finish_scheme_request(&self, token: $crate::SchemeToken, response: $crate::SchemeResponse) {
                self.send($crate::host::Command::Scheme(token, response));
            }

            /// Puts `text` on the engine's clipboard, which every page pastes from: the consumer hands it the desktop's
            /// text
            /// before a paste reaches a page. Only text: a page never gets files from the clipboard.
            pub fn set_clipboard(&self, text: String) {
                self.send($crate::host::Command::Clipboard(text));
            }

            /// Answers a [`crate::OptionMenu`]: the row at `index` is chosen (the select changes, the popup closes), or
            /// with
            /// `None` the popup closes unchanged.
            pub fn choose_option(&self, token: $crate::MenuToken, index: Option<u32>) {
                self.send($crate::host::Command::OptionMenu(token, index));
            }

            /// Runs an editing command on the page's focused content (a context menu's Copy, Paste, Select All).
            pub fn editing_command(&self, view: $crate::ViewId, command: $crate::EditingCommand) {
                self.view(view, $crate::host::ViewCommand::Editing(command));
            }

            /// An input method's composition in the focused field: `text` (empty ends it) with the caret `cursor`
            /// characters
            /// in.
            pub fn ime_preedit(&self, view: $crate::ViewId, text: String, cursor: u32) {
                self.view(view, $crate::host::ViewCommand::ImePreedit { text, cursor });
            }

            /// Text an input method committed, inserted into the focused field (ending a composition).
            pub fn ime_commit(&self, view: $crate::ViewId, text: String) {
                self.view(view, $crate::host::ViewCommand::ImeCommit(text));
            }
        }
    };
}
