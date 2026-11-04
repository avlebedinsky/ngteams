//! Local files staged in the editor. A drop never changes the text or sends a message.

use std::path::{Path, PathBuf};

use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement as _, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, div, px, rgb,
};
use gpui_component::Sizable as _;
use teams_proto::chat::FileKind;

use super::Compose;
use crate::files::{kind_colour, kind_glyph};
use crate::icons::{Glyph, icon_button, selected_by};
use crate::theme::Palette;

/// A local file waiting for Send, kept apart from the editor's rich text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attachment {
    pub id: u64,
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    pub state: AttachmentState,
}

/// Uploads are private until Send; a failed file remains in the draft with Retry and Remove.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttachmentState {
    Uploading,
    Ready(teams_proto::UploadedFile),
    Failed(gpui::SharedString),
}

impl Attachment {
    /// Reads only metadata: directories, devices, missing files and non-local paths aren't attachments.
    ///
    /// # Errors
    /// Rejects unreadable names, missing files, non-files, relative paths and files above the upload limit.
    pub fn of(path: &Path) -> Result<Self, &'static str> {
        if !path.is_absolute() {
            return Err("Choose a file on this device.");
        }
        let metadata = path
            .metadata()
            .map_err(|_| "That file can't be read. Choose it again.")?;
        if !metadata.is_file() {
            return Err("Folders can't be attached. Choose the files inside the folder.");
        }
        if metadata.len() > teams_proto::MAX_ATTACHMENT_SIZE {
            return Err("Choose a file smaller than 250 MB.");
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .ok_or("That file's name can't be read. Rename it and choose it again.")?;
        Ok(Self {
            id: 0,
            path: path.to_owned(),
            name: name.to_owned(),
            size: metadata.len(),
            state: AttachmentState::Uploading,
        })
    }
    fn complete(&mut self, answer: Result<teams_proto::UploadedFile, teams_core::EngineError>) {
        self.state = match answer {
            Ok(uploaded) => {
                uploaded.name().clone_into(&mut self.name);
                self.size = uploaded.size();
                AttachmentState::Ready(uploaded)
            }
            Err(error) => AttachmentState::Failed(error.to_string().into()),
        };
    }
}

/// Whether an operating-system file drag includes a file the editor can take.
#[must_use]
pub fn supported(paths: &gpui::ExternalPaths) -> bool {
    paths.paths().iter().any(|path| Attachment::of(path).is_ok())
}

fn size_label(size: u64) -> String {
    if size < 1024 {
        format!("{size} B")
    } else if size < 1024 * 1024 {
        format!("{} KB", size.div_ceil(1024))
    } else {
        format!("{} MB", size.div_ceil(1024 * 1024))
    }
}

impl Compose {
    /// Adds files to this chat's draft without replacing its text, reply, or files already chosen.
    pub fn attach_files(&mut self, paths: &[PathBuf], window: &mut gpui::Window, cx: &mut Context<Self>) {
        if !self.accepts_files() {
            return;
        }
        self.problem = None;
        for path in paths {
            if self.attachments.iter().any(|file| file.path == *path) {
                continue;
            }
            match Attachment::of(path) {
                Ok(mut file) => {
                    self.next_attachment += 1;
                    file.id = self.next_attachment;
                    self.attachments.push(file);
                    self.upload_attachment(self.next_attachment, cx);
                }
                Err(error) => self.problem = Some(error.into()),
            }
        }
        self.focus(window, cx);
        cx.notify();
    }

    /// Files belong to new messages and replies; an existing message's files aren't edited here.
    #[must_use]
    pub fn accepts_files(&self) -> bool {
        self.read_only.is_none() && !matches!(self.mode, super::Mode::Edit { .. })
    }

    #[must_use]
    pub fn attachments(&self) -> &[Attachment] {
        &self.attachments
    }

    fn upload_attachment(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(file) = self.attachments.iter_mut().find(|file| file.id == id) else {
            return;
        };
        file.state = AttachmentState::Uploading;
        let reply = self.core.upload_file(file.path.clone());
        cx.spawn(async move |this, cx| {
            let answer = reply.await;
            let _ = this.update(cx, |this, cx| {
                if let Some(file) = this.attachments.iter_mut().find(|file| file.id == id) {
                    file.complete(answer);
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn files_ready(&self) -> bool {
        self.attachments
            .iter()
            .all(|file| matches!(file.state, AttachmentState::Ready(_)))
    }

    pub(super) fn attachment_cards(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.attachments.is_empty() {
            return None;
        }
        Some(
            div()
                .debug_selector(|| "compose-attachments".to_owned())
                .id("compose-attachments")
                .max_h(px(260.0))
                .overflow_y_scroll()
                .flex()
                .flex_wrap()
                .gap_2()
                .children(
                    self.attachments
                        .iter()
                        .enumerate()
                        .map(|(index, file)| Self::attachment_card(index, file, palette, cx)),
                )
                .into_any_element(),
        )
    }

    fn attachment_card(index: usize, file: &Attachment, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let (status, colour) = match &file.state {
            AttachmentState::Uploading => ("Uploading…".to_owned(), palette.brand),
            AttachmentState::Ready(_) => (size_label(file.size), palette.muted_foreground),
            AttachmentState::Failed(error) => (error.to_string(), crate::theme::DANGER),
        };
        let kind = FileKind::of(file.name.rsplit_once('.').map_or("", |(_, extension)| extension));
        div()
            .debug_selector(move || format!("compose-attachment-{index}"))
            .w(px(316.0))
            .max_w_full()
            .flex()
            .flex_col()
            .border_1()
            .border_color(rgb(palette.border))
            .rounded_md()
            .bg(rgb(palette.surface))
            .child(Self::attachment_header(index, file, kind, palette, cx))
            .child(
                div()
                    .h(px(150.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(rgb(palette.background))
                    .child(
                        kind_glyph(kind)
                            .icon()
                            .size(px(40.0))
                            .text_color(rgb(kind_colour(kind))),
                    ),
            )
            .child(div().px_3().py_1().text_xs().text_color(rgb(colour)).child(status))
            .into_any_element()
    }

    fn attachment_header(
        index: usize,
        file: &Attachment,
        kind: FileKind,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = file.id;
        let remove = selected_by(
            format!("compose-attachment-remove-{index}"),
            icon_button(("attachment-remove", index), Glyph::X, "Remove attachment")
                .xsmall()
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.attachments.retain(|file| file.id != id);
                    this.focus(window, cx);
                    cx.notify();
                })),
        );
        let retry = matches!(file.state, AttachmentState::Failed(_)).then(|| {
            selected_by(
                format!("compose-attachment-retry-{index}"),
                icon_button(("attachment-retry", index), Glyph::REFRESH, "Retry upload")
                    .xsmall()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.upload_attachment(id, cx);
                        this.focus(window, cx);
                    })),
            )
        });
        div()
            .flex()
            .items_center()
            .gap_2()
            .p_3()
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(
                kind_glyph(kind)
                    .icon()
                    .size(px(24.0))
                    .text_color(rgb(kind_colour(kind))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .truncate()
                    .child(file.name.clone()),
            )
            .children(retry)
            .child(remove)
            .into_any_element()
    }
}
