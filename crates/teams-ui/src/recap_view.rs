//! A meeting chat's Recap tab, as Teams draws it (`crate::recap` works out what it shows):
//!
//! - **The top row**: the meeting's line (its day and span) in a box with its menu, "Part 1", "Part 2" when the
//!   meeting was recorded more than once, then Share (its menu: Copy link, the recap's Teams link), Watch in browser
//!   (the recording in Stream, for a part with a recording) and "More options" (Open in Teams on the web, Copy link).
//! - **A recorded part**: the recording played here ([`VideoPlayer`], filling its column at 16:9), under it the
//!   Speakers chip with Teams' Copilot upsell (the account has no Microsoft 365 Copilot, as both accounts Teams was
//!   looked at with); beside it the files shared in the chat and the panes: AI summary (the same upsell) and the
//!   transcript. A part with only a transcript shows the files and the panes across the whole width.
//! - **The transcript**: Teams' note that it is AI-generated, who started transcription, each speaker's turn (their
//!   initials, name and time into the recording) with its paragraphs; the paragraph being spoken is highlighted and
//!   kept in view while the recording plays, and a click on one plays the recording from there. Search finds the
//!   paragraphs that hold its words ("1/3", Enter or the arrows go on, the match highlighted); Download saves the
//!   `WebVTT` as Teams' "Download as .vtt" does.
//! - **Its states**: a spinner while the recap or a transcript is read; one that can't be read says so and offers to try
//!   again, or Teams on the web; a meeting that left nothing says so.
//!
//! The engine reads everything ([`Core::meeting_contents`], [`Core::meeting_transcript`], the chat's stored messages
//! for its shared files); no view builds a request.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Entity, FocusHandle, FontWeight, HighlightStyle,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, StyledText, Subscription, Task, Window, div, px, rgb,
};
use gpui_component::Sizable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use teams_proto::chat::SharedFile;
use teams_proto::recap::{RecapPart, recap_link, recap_parts};
use teams_proto::transcript::Turn;

use crate::bridge::Core;
use crate::clock;
use crate::icons::{Glyph, icon_button, selected_by};
use crate::inline::PictureSource;
use crate::people;
use crate::recap::{
    AI_NOTICE, NO_FILES, Search, SharedItem, Transcript, UPGRADE_COPILOT, UPGRADE_LINK, local_session_line, part_label,
    row_of, shows_parts, transcript_file_name,
};
use crate::theme::{self, Palette};
use crate::video_player::{VideoPlayer, clock as offset};

/// How far a turn's paragraphs stand in from its speaker's initials: past them, under the name.
const TURN_INDENT: f32 = 36.0;

/// The words a recap that couldn't be read shows.
pub const RECAP_UNREAD: &str = "The recap couldn't be loaded.";
/// The words a meeting that left nothing shows.
pub const NOTHING_LEFT: &str = "This meeting wasn't recorded or transcribed.";
/// The words a recap shows while a recording of the meeting is still being saved.
pub const SAVING: &str = "Saving recording\u{2026} It shows here once it's ready.";
/// The words a transcript that couldn't be read shows.
pub const TRANSCRIPT_UNREAD: &str = "The transcript couldn't be loaded.";

/// The panes beside the recording.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pane {
    AiSummary,
    Transcript,
}

/// What the recap read.
enum Contents {
    Reading,
    Failed,
    Read(Vec<RecapPart>),
}

/// A transcript as read.
enum Read {
    Reading,
    Failed,
    Shown { vtt: Arc<str>, transcript: Transcript },
}

/// A meeting chat's Recap tab (see the module docs).
pub struct RecapView {
    core: Arc<dyn Core>,
    conversation: String,
    title: SharedString,
    contents: Contents,
    selected: usize,
    pane: Pane,
    transcripts: HashMap<String, Read>,
    /// Each recording's player, by its sharing link (a part's place can change when the recap is read again).
    players: HashMap<String, (Entity<VideoPlayer>, Subscription)>,
    /// The paragraph being spoken, as last kept in view.
    spoken: Option<(usize, usize)>,
    /// The recording asked for ("View recap" on its card) before the parts were read.
    wanted: Option<String>,
    search_box: Entity<InputState>,
    search: Option<Search>,
    /// What was searched for: searched again once the transcript it was asked of arrives.
    query: String,
    files: Vec<SharedItem>,
    /// A recording of the meeting is still being saved (its chat says so; MCPS doesn't list it yet).
    saving: bool,
    /// Who the transcript says started it: the meeting's organizer's name.
    organizer: Option<String>,
    scroll: ScrollHandle,
    note: Option<SharedString>,
    focus: FocusHandle,
    _typed: Subscription,
    reading: Option<Task<()>>,
}

impl RecapView {
    /// The recap of the meeting whose chat is `conversation`, titled `title`; its read starts now.
    pub fn new(
        core: Arc<dyn Core>,
        conversation: String,
        title: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search_box = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let typed = cx.subscribe(&search_box, |this, search_box, event, cx| {
            this.searched(&search_box, event, cx);
        });
        let mut view = Self {
            core,
            conversation,
            title,
            contents: Contents::Reading,
            selected: 0,
            pane: Pane::AiSummary,
            transcripts: HashMap::new(),
            players: HashMap::new(),
            spoken: None,
            wanted: None,
            search_box,
            search: None,
            query: String::new(),
            files: Vec::new(),
            saving: false,
            organizer: None,
            scroll: ScrollHandle::new(),
            note: None,
            focus: cx.focus_handle(),
            _typed: typed,
            reading: None,
        };
        view.read(cx);
        view.read_files(cx);
        view
    }

    /// The chat the recap is of.
    #[must_use]
    pub fn conversation(&self) -> &str {
        &self.conversation
    }

    /// The pane shown beside the recording.
    #[must_use]
    pub fn pane(&self) -> Pane {
        self.pane
    }

    /// The part shown, counted from 0.
    #[must_use]
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// The parts read, once they were.
    #[must_use]
    pub fn parts(&self) -> Option<&[RecapPart]> {
        match &self.contents {
            Contents::Read(parts) => Some(parts),
            Contents::Reading | Contents::Failed => None,
        }
    }

    /// The search as it stands, when one was made.
    #[must_use]
    pub fn search(&self) -> Option<&Search> {
        self.search.as_ref()
    }

    /// The meeting's name changed (the chat list read it): the recap's file names follow it.
    pub fn set_title(&mut self, title: SharedString, cx: &mut Context<Self>) {
        self.title = title;
        cx.notify();
    }

    /// Pauses its recordings: the tab, or the chat, was put away (a player out of sight keeps no sound going).
    pub fn pause(&mut self, cx: &mut Context<Self>) {
        for (player, _) in self.players.values() {
            player.update(cx, VideoPlayer::pause);
        }
    }

    /// The tab shown again (or "View recap" clicked): a recap that couldn't be read, or that held nothing yet (a
    /// recording still being saved, which MCPS lists only once it is), is read again with the chat's files.
    pub fn read_again_if_empty(&mut self, cx: &mut Context<Self>) {
        let empty = match &self.contents {
            Contents::Reading => false,
            Contents::Failed => true,
            Contents::Read(parts) => parts.is_empty(),
        };
        if empty {
            self.read(cx);
            self.read_files(cx);
        }
    }

    /// Reads the meeting's contents (again).
    pub fn read(&mut self, cx: &mut Context<Self>) {
        self.contents = Contents::Reading;
        let reply = self.core.meeting_contents(self.conversation.clone());
        self.reading = Some(cx.spawn(async move |this, cx| {
            let answer = reply.await;
            let _ = this.update(cx, |this, cx| {
                this.contents = match answer {
                    Ok(contents) => Contents::Read(recap_parts(&contents)),
                    Err(_) => Contents::Failed,
                };
                // read again (Refresh), the parts may be fewer: a part gone takes its player with it
                let parts = this.parts().unwrap_or_default();
                let recordings: Vec<String> = parts.iter().filter_map(|part| part.recording.clone()).collect();
                if this.selected >= parts.len() {
                    this.selected = 0;
                }
                this.players.retain(|link, _| recordings.contains(link));
                if let Some(recording) = this.wanted.take() {
                    this.show_recording(recording, cx);
                }
                this.organizer_name(cx);
                this.read_transcript(cx);
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// The files the chat's stored messages shared, for the "Shared files" card, and whether a recording is still
    /// being saved.
    fn read_files(&mut self, cx: &mut Context<Self>) {
        let task = crate::shared_view::read_messages(Arc::clone(&self.core), self.conversation.clone(), cx);
        cx.spawn(async move |this, cx| {
            let messages = task.await;
            let _ = this.update(cx, |this, cx| {
                this.saving = crate::recap::recording_saving(&messages);
                this.files = crate::recap::shared_items(&messages)
                    .into_iter()
                    .filter(SharedItem::is_file)
                    .collect();
                cx.notify();
            });
        })
        .detach();
    }

    /// Asks for the organizer's name, who the transcript says started it.
    fn organizer_name(&mut self, cx: &mut Context<Self>) {
        let Some(organizer) = self.part().and_then(|part| part.organizer_id.clone()) else {
            return;
        };
        let reply = self.core.person(format!("8:orgid:{organizer}"));
        cx.spawn(async move |this, cx| {
            if let Ok(person) = reply.await {
                let _ = this.update(cx, |this, cx| {
                    this.organizer = person.name;
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// The part shown.
    fn part(&self) -> Option<&RecapPart> {
        self.parts()?.get(self.selected)
    }

    /// Shows part `index`: its recording, its transcript (read now if it wasn't), the other parts' paused.
    pub fn select_part(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.parts().is_none_or(|parts| index >= parts.len()) {
            return;
        }
        self.pause(cx);
        self.selected = index;
        self.spoken = None;
        self.search = None;
        self.query.clear();
        self.read_transcript(cx);
        cx.notify();
    }

    /// Shows the part of the recording whose sharing link is `recording` (once the parts are read).
    pub fn show_recording(&mut self, recording: String, cx: &mut Context<Self>) {
        let Some(parts) = self.parts() else {
            self.wanted = Some(recording);
            return;
        };
        if let Some(index) = parts
            .iter()
            .position(|part| part.recording.as_deref() == Some(recording.as_str()))
        {
            self.select_part(index, cx);
        }
    }

    /// Shows `pane`.
    pub fn show_pane(&mut self, pane: Pane, cx: &mut Context<Self>) {
        self.pane = pane;
        self.read_transcript(cx);
        cx.notify();
    }

    /// Reads the shown part's transcript while its pane shows, when it has one not read yet (or whose read failed and
    /// is asked again).
    fn read_transcript(&mut self, cx: &mut Context<Self>) {
        if self.pane != Pane::Transcript {
            return;
        }
        let Some(location) = self.part().and_then(|part| part.transcript.clone()) else {
            return;
        };
        if matches!(
            self.transcripts.get(&location),
            Some(Read::Reading | Read::Shown { .. })
        ) {
            return;
        }
        self.transcripts.insert(location.clone(), Read::Reading);
        let reply = self.core.meeting_transcript(location.clone());
        cx.spawn(async move |this, cx| {
            let answer = reply.await;
            let _ = this.update(cx, |this, cx| {
                let read = answer.ok().and_then(|vtt| Some((Transcript::of(&vtt)?, vtt))).map_or(
                    Read::Failed,
                    |(transcript, vtt)| Read::Shown {
                        vtt: vtt.into(),
                        transcript,
                    },
                );
                this.transcripts.insert(location, read);
                // a search asked before the transcript came
                if !this.query.is_empty() && this.search.is_none() {
                    let query = this.query.clone();
                    this.find(&query);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The shown part's transcript, once read.
    fn transcript(&self) -> Option<&Transcript> {
        let location = self.part()?.transcript.as_ref()?;
        match self.transcripts.get(location)? {
            Read::Shown { transcript, .. } => Some(transcript),
            Read::Reading | Read::Failed => None,
        }
    }

    /// The shown part's player, made when first asked for.
    fn player(&mut self, cx: &mut Context<Self>) -> Option<Entity<VideoPlayer>> {
        let index = self.selected;
        let link = self.part()?.recording.clone()?;
        if let Some((player, _)) = self.players.get(&link) {
            return Some(player.clone());
        }
        let file = SharedFile {
            name: format!("{}.mp4", self.title),
            extension: "mp4".to_owned(),
            share_url: Some(link.clone()),
            object_url: None,
            preview: None,
        };
        let source = PictureSource::new(Arc::clone(&self.core));
        let id = format!("recap-{index}");
        let player = cx.new(|cx| VideoPlayer::new(id, file, source, cx).filling());
        let watched = cx.observe(&player, |this, player, cx| this.follow(&player, cx));
        self.players.insert(link, (player.clone(), watched));
        Some(player)
    }

    /// The recording moved on: the paragraph spoken now is highlighted and kept in view.
    fn follow(&mut self, player: &Entity<VideoPlayer>, cx: &mut Context<Self>) {
        let Some(at) = player.read(cx).position() else { return };
        let spoken = self.transcript().and_then(|transcript| transcript.spoken_at(at));
        if spoken != self.spoken {
            self.spoken = spoken;
            if let (Some(at), Some(transcript)) = (spoken, self.transcript()) {
                self.scroll.scroll_to_item(row_of(&transcript.turns, at));
            }
            cx.notify();
        }
    }

    /// A paragraph clicked: the recording plays from where it starts.
    pub fn play_from(&mut self, at: Duration, cx: &mut Context<Self>) {
        if let Some(player) = self.player(cx) {
            player.update(cx, |player, cx| player.play_from(at, cx));
        }
    }

    /// The search box changed or Enter was pressed in it.
    fn searched(&mut self, search_box: &Entity<InputState>, event: &InputEvent, cx: &mut Context<Self>) {
        let query = search_box.read(cx).value().to_string();
        match event {
            InputEvent::PressEnter { shift, .. } => {
                match self.search.as_mut().filter(|search| search.query == query.trim()) {
                    Some(search) if *shift => search.previous(),
                    Some(search) => search.next(),
                    None => self.find(&query),
                }
                self.reveal_match();
            }
            InputEvent::Change => {
                self.find(&query);
                self.reveal_match();
            }
            _ => return,
        }
        cx.notify();
    }

    /// Searches the shown transcript for `query` (a blank one ends the search).
    pub fn find(&mut self, query: &str) {
        query.trim().clone_into(&mut self.query);
        self.search = if query.trim().is_empty() {
            None
        } else {
            self.transcript().map(|transcript| Search::new(transcript, query))
        };
    }

    /// Goes to the next (or, `back`, the previous) match.
    pub fn step_match(&mut self, back: bool, cx: &mut Context<Self>) {
        if let Some(search) = self.search.as_mut() {
            if back {
                search.previous();
            } else {
                search.next();
            }
        }
        self.reveal_match();
        cx.notify();
    }

    fn reveal_match(&self) {
        if let (Some(at), Some(transcript)) = (self.search.as_ref().and_then(Search::shown), self.transcript()) {
            self.scroll.scroll_to_item(row_of(&transcript.turns, at));
        }
    }

    /// Download as .vtt: the transcript saved where the user chooses (the downloads folder without a chooser).
    pub fn download(&mut self, cx: &mut Context<Self>) {
        let Some(location) = self.part().and_then(|part| part.transcript.clone()) else {
            return;
        };
        let Some(Read::Shown { vtt, .. }) = self.transcripts.get(&location) else {
            return;
        };
        let vtt = Arc::clone(vtt);
        let name = transcript_file_name(&self.title);
        let folder = crate::picture_viewer::downloads();
        let chosen = cx.prompt_for_new_path(&folder, Some(&name));
        cx.spawn(async move |this, cx| {
            let target = match chosen.await {
                Ok(Ok(Some(path))) => path,
                Ok(Ok(None)) => return,
                Ok(Err(_)) | Err(_) => crate::picture_viewer::free_name(&folder, &name),
            };
            let written = cx
                .background_executor()
                .spawn({
                    let target = target.clone();
                    async move { std::fs::write(&target, vtt.as_bytes()) }
                })
                .await;
            let shown = target.file_name().map_or_else(
                || target.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            );
            let note = match written {
                Ok(()) => format!("Saved as {shown}"),
                Err(error) => format!("Couldn't save {shown}: {error}"),
            };
            let _ = this.update(cx, |this, cx| {
                this.note = Some(note.into());
                cx.notify();
            });
        })
        .detach();
    }

    /// The link Share → Copy link copies: the recap's Teams link for the part shown.
    fn link(&self) -> Option<String> {
        recap_link(self.part()?, &self.conversation)
    }
}

impl Render for RecapView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let body = match &self.contents {
            Contents::Reading => centred(spinner(palette), palette).into_any_element(),
            Contents::Failed => self.unread(palette, cx),
            Contents::Read(parts) if parts.is_empty() => {
                let words = if self.saving { SAVING } else { NOTHING_LEFT };
                centred(empty_state(Glyph::VIDEO, words, palette), palette).into_any_element()
            }
            Contents::Read(_) => self.recap(window, palette, cx),
        };
        div()
            .debug_selector(|| "recap-view".to_owned())
            .id("recap-view")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(palette.background))
            .child(body)
    }
}

/// The parts of the view.
impl RecapView {
    /// The recap that couldn't be read: what happened, Try again and Teams on the web.
    fn unread(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let again = Button::new("recap-retry")
            .label("Try again")
            .primary()
            .on_click(cx.listener(|this, _, _, cx| this.read(cx)));
        let web = teams_proto::chat::web_chat_link(&self.conversation);
        let open = Button::new("recap-open-web")
            .label(crate::chat_view::OPEN_ON_THE_WEB)
            .outline()
            .on_click(move |_, _, cx| cx.open_url(&web));
        let state = empty_state(Glyph::CIRCLE_ALERT, RECAP_UNREAD, palette).child(
            div()
                .flex()
                .gap_2()
                .child(selected_by("recap-retry", again))
                .child(selected_by("recap-open-web", open)),
        );
        centred(state, palette).into_any_element()
    }

    fn recap(&mut self, window: &mut Window, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let top = self.top_row(palette, cx);
        let recorded = self.part().is_some_and(|part| part.recording.is_some());
        let side = div()
            .flex()
            .flex_col()
            .gap_2()
            .min_h_0()
            .child(self.files_card(palette, cx))
            .child(self.panes_card(window, palette, cx));
        let body = if recorded {
            let player = self.player(cx);
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .gap_2()
                .child(recording_card(player, palette))
                .child(side.w(px(400.0)).flex_none())
        } else {
            div().flex_1().min_h_0().flex().flex_col().child(side.flex_1())
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_2()
            .px_3()
            .pb_3()
            .child(top)
            .children(self.saving.then(|| {
                div()
                    .debug_selector(|| "recap-saving".to_owned())
                    .text_sm()
                    .text_color(rgb(palette.muted_foreground))
                    .child(SAVING)
            }))
            .child(body)
            .children(self.note.clone().map(|note| {
                div()
                    .debug_selector(|| "recap-note".to_owned())
                    .text_xs()
                    .text_color(rgb(palette.muted_foreground))
                    .child(note)
            }))
            .into_any_element()
    }

    /// The meeting's line, the parts, Share, Watch in browser, More options.
    fn top_row(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let parts = self.parts().unwrap_or_default();
        let line = local_session_line(parts, clock::cycle(cx)).unwrap_or_else(|| self.title.to_string());
        let face = div()
            .w(px(360.0))
            .flex()
            .items_center()
            .gap_2()
            .child(div().flex_1().min_w_0().truncate().text_left().child(line.clone()))
            .child(Glyph::CHEVRON_DOWN.icon().size(px(14.0)));
        // the meeting's sessions, as Teams lists them: this chat's meeting, checked
        let session = Button::new("recap-session")
            .outline()
            .child(face)
            .dropdown_menu(move |menu, _, _| {
                let line = line.clone();
                menu.item(PopupMenuItem::element(move |_, _| {
                    div()
                        .debug_selector(|| "recap-session-row".to_owned())
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(Glyph::CHECK.icon().size(px(14.0)))
                        .child(line.clone())
                }))
            });
        let session = selected_by("recap-session", session)
            .flex_shrink(1.0)
            .min_w(px(160.0))
            .bg(rgb(palette.surface));
        let tabs = shows_parts(parts).then(|| self.part_tabs(parts.len(), palette, cx));
        div()
            .h(px(48.0))
            .flex_none()
            .flex()
            .items_center()
            .gap_3()
            .child(session)
            .children(tabs)
            .child(div().flex_1())
            .children(self.actions(cx))
            .into_any_element()
    }

    fn part_tabs(&self, count: usize, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let tab = |index: usize, cx: &mut Context<Self>| {
            let chosen = index == self.selected;
            div()
                .id(SharedString::from(format!("recap-part-{index}")))
                .debug_selector(move || format!("recap-part-{index}"))
                .h(px(32.0))
                .px_1()
                .flex()
                .items_center()
                .gap_1()
                .cursor_pointer()
                .border_b_2()
                .border_color(if chosen {
                    rgb(palette.brand)
                } else {
                    gpui::transparent_black().into()
                })
                .text_color(rgb(if chosen {
                    palette.foreground
                } else {
                    palette.muted_foreground
                }))
                .when(chosen, |tab| tab.font_weight(FontWeight::SEMIBOLD))
                .child(Glyph::PLAY_MEDIA.icon().size(px(14.0)))
                .child(part_label(index))
                .on_click(cx.listener(move |this, _, _, cx| this.select_part(index, cx)))
        };
        let mut row = div()
            .flex()
            .items_center()
            .gap_3()
            .pl_3()
            .border_l_1()
            .border_color(rgb(palette.border));
        for index in 0..count {
            row = row.child(tab(index, cx));
        }
        row.into_any_element()
    }

    /// Share, Watch in browser (a recorded part) and More options.
    fn actions(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut actions = Vec::new();
        let link = self.link();
        if let Some(link) = link.clone() {
            let share = Button::new("recap-share")
                .icon(Glyph::SHARE.icon())
                .label("Share")
                .ghost()
                .dropdown_menu(move |menu, _, _| {
                    let copied = link.clone();
                    menu.item(
                        menu_row("recap-share-copy", Glyph::LINK, "Copy link").on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()));
                        }),
                    )
                });
            actions.push(selected_by("recap-share", share).into_any_element());
        }
        if let Some(watch) = self.part().and_then(|part| part.recording.clone()) {
            let button = Button::new("recap-watch")
                .icon(Glyph::EXTERNAL_LINK.icon())
                .label("Watch in browser")
                .ghost()
                .on_click(move |_, _, cx| cx.open_url(&watch));
            actions.push(selected_by("recap-watch", button).into_any_element());
        }
        let web = link.unwrap_or_else(|| teams_proto::chat::web_chat_link(&self.conversation));
        let me = cx.entity().downgrade();
        let more = icon_button("recap-more", Glyph::MORE, "More options").dropdown_menu(move |menu, _, _| {
            let (open, copied, again) = (web.clone(), web.clone(), me.clone());
            menu.item(
                menu_row(
                    "recap-more-web",
                    Glyph::EXTERNAL_LINK,
                    crate::chat_view::OPEN_ON_THE_WEB,
                )
                .on_click(move |_, _, cx| cx.open_url(&open)),
            )
            .item(
                menu_row("recap-more-copy", Glyph::LINK, "Copy link")
                    .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()))),
            )
            .item(
                menu_row("recap-more-refresh", Glyph::REFRESH, "Refresh").on_click(move |_, _, cx| {
                    let _ = again.update(cx, Self::read);
                }),
            )
        });
        actions.push(selected_by("recap-more", more).into_any_element());
        actions
    }

    /// "Shared files": the files the chat shared, each opened in the browser; Teams' words when there are none.
    fn files_card(&self, palette: Palette, cx: &App) -> AnyElement {
        let _ = cx;
        let files: Vec<AnyElement> = self
            .files
            .iter()
            .take(8)
            .enumerate()
            .map(|(index, file)| file_chip(index, file, palette))
            .collect();
        let shown = if files.is_empty() {
            div()
                .debug_selector(|| "recap-no-files".to_owned())
                .child(NO_FILES)
                .into_any_element()
        } else {
            div().flex().flex_wrap().gap_2().children(files).into_any_element()
        };
        card(palette)
            .debug_selector(|| "recap-files".to_owned())
            .flex_none()
            .p_4()
            .gap_2()
            .child(div().font_weight(FontWeight::SEMIBOLD).child("Shared files"))
            .child(shown)
            .into_any_element()
    }

    /// The panes' pills and the pane shown.
    fn panes_card(&mut self, window: &mut Window, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let has_transcript = self.part().is_some_and(|part| part.transcript.is_some());
        let pill = |pane: Pane, label: &'static str, key: &'static str, cx: &mut Context<Self>| {
            let chosen = self.pane == pane;
            div()
                .id(SharedString::from(format!("recap-pane-{key}")))
                .debug_selector(move || format!("recap-pane-{key}"))
                .h(px(32.0))
                .px_3()
                .flex()
                .items_center()
                .rounded_full()
                .border_1()
                .cursor_pointer()
                .border_color(rgb(if chosen { palette.brand } else { palette.border }))
                .bg(rgb(if chosen { palette.selected } else { palette.surface }))
                .text_color(rgb(if chosen { palette.brand } else { palette.foreground }))
                .when(chosen, |pill| pill.font_weight(FontWeight::SEMIBOLD))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| this.show_pane(pane, cx)))
        };
        let mut pills = div()
            .flex()
            .gap_2()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(rgb(palette.border));
        pills = pills.child(pill(Pane::AiSummary, "AI summary", "summary", cx));
        if has_transcript {
            pills = pills.child(pill(Pane::Transcript, "Transcript", "transcript", cx));
        }
        let shown = match self.pane {
            Pane::Transcript if has_transcript => self.transcript_pane(window, palette, cx),
            Pane::AiSummary | Pane::Transcript => summary_upsell(palette),
        };
        card(palette)
            .debug_selector(|| "recap-panes".to_owned())
            .flex_1()
            .min_h_0()
            .child(pills)
            .child(div().flex_1().min_h_0().flex().flex_col().child(shown))
            .into_any_element()
    }

    fn transcript_pane(&mut self, window: &mut Window, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let _ = window;
        let location = self.part().and_then(|part| part.transcript.clone()).unwrap_or_default();
        let toolbar = self.transcript_toolbar(palette, cx);
        let body = match self.transcripts.get(&location) {
            None | Some(Read::Reading) => centred(spinner(palette), palette).into_any_element(),
            Some(Read::Failed) => {
                let again = Button::new("recap-transcript-retry")
                    .label("Try again")
                    .outline()
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(location) = this.part().and_then(|part| part.transcript.clone()) {
                            this.transcripts.remove(&location);
                        }
                        this.read_transcript(cx);
                        cx.notify();
                    }));
                let state = empty_state(Glyph::CIRCLE_ALERT, TRANSCRIPT_UNREAD, palette)
                    .child(selected_by("recap-transcript-retry", again));
                centred(state, palette).into_any_element()
            }
            Some(Read::Shown { transcript, .. }) => {
                let turns = transcript.turns.clone();
                self.transcript_rows(&turns, palette, cx)
            }
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(toolbar)
            .child(body)
            .into_any_element()
    }

    /// Search (and its matches' count and arrows) and Download.
    fn transcript_toolbar(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let search = div().debug_selector(|| "recap-search".to_owned()).flex_1().child(
            Input::new(&self.search_box)
                .prefix(Glyph::SEARCH.icon().size(px(14.0)))
                .small(),
        );
        let matches = self.search.as_ref().map(|search| {
            let label = search.label();
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(selected_by(
                    "recap-search-previous",
                    icon_button("recap-search-previous", Glyph::CHEVRON_UP, "Previous match")
                        .small()
                        .on_click(cx.listener(|this, _, _, cx| this.step_match(true, cx))),
                ))
                .child(
                    div()
                        .debug_selector(move || format!("recap-search-count {label}"))
                        .text_xs()
                        .text_color(rgb(palette.muted_foreground))
                        .child(search.label()),
                )
                .child(selected_by(
                    "recap-search-next",
                    icon_button("recap-search-next", Glyph::CHEVRON_DOWN, "Next match")
                        .small()
                        .on_click(cx.listener(|this, _, _, cx| this.step_match(false, cx))),
                ))
        });
        let me = cx.entity().downgrade();
        let download = Button::new("recap-download")
            .icon(Glyph::DOWNLOAD.icon())
            .label("Download")
            .ghost()
            .small()
            .dropdown_menu(move |menu, _, _| {
                let me = me.clone();
                menu.item(
                    menu_row("recap-download-vtt", Glyph::FILE, "Download as .vtt").on_click(move |_, _, cx| {
                        let _ = me.update(cx, Self::download);
                    }),
                )
            });
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .px_4()
            .py_2()
            .child(search)
            .children(matches)
            .child(selected_by("recap-download", download))
            .into_any_element()
    }

    /// The transcript's rows: the note, who started it, each turn, and for a transcript alone who stopped it.
    fn transcript_rows(&self, turns: &[Turn], palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let who = self.organizer.clone().unwrap_or_else(|| "The organizer".to_owned());
        let only_transcript = self.part().is_some_and(|part| part.recording.is_none());
        let mut rows: Vec<AnyElement> = vec![
            event_row("recap-ai-notice", Glyph::INFO, AI_NOTICE.to_owned(), palette),
            event_row(
                "recap-started",
                Glyph::TRANSCRIPT,
                format!("{who} started transcription"),
                palette,
            ),
        ];
        for (index, turn) in turns.iter().enumerate() {
            rows.extend(self.turn_rows(index, turn, palette, cx));
        }
        if only_transcript {
            rows.push(event_row(
                "recap-stopped",
                Glyph::TRANSCRIPT,
                format!("{who} stopped transcription"),
                palette,
            ));
        }
        div()
            .id("recap-transcript")
            .debug_selector(|| "recap-transcript".to_owned())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .px_4()
            .pb_4()
            .children(rows)
            .into_any_element()
    }

    /// One speaker's turn, a row each (the transcript is followed by paragraph): their initials, name and time, then
    /// its paragraphs under them, each played from on a click.
    fn turn_rows(&self, index: usize, turn: &Turn, palette: Palette, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let name = turn.speaker.clone().unwrap_or_else(|| "Speaker".to_owned());
        let avatar = people::avatar(
            Some(&SharedString::from(name.clone())),
            None,
            gpui_component::Size::Small,
        );
        let header = div()
            .debug_selector(move || format!("recap-turn-{index}"))
            .pt_3()
            .flex()
            .items_center()
            .gap_3()
            .child(div().w(px(TURN_INDENT - 12.0)).flex_none().child(avatar))
            .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).child(name))
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(palette.muted_foreground))
                    .child(offset(turn.start)),
            );
        let query = self.search.as_ref().map(|search| search.query.clone());
        let paragraphs = turn.paragraphs.iter().enumerate().map(|(at, paragraph)| {
            let spoken = self.spoken == Some((index, at));
            let current = self.search.as_ref().and_then(|search| search.current_range(index, at));
            let start = paragraph.start;
            let text = highlighted(&paragraph.text, query.as_deref(), current.as_ref(), palette);
            div()
                .pl(px(TURN_INDENT - 4.0))
                .pt_1()
                .child(
                    div()
                        .id(SharedString::from(format!("recap-paragraph-{index}-{at}")))
                        .debug_selector(move || {
                            format!("recap-paragraph-{index}-{at}{}", if spoken { " spoken" } else { "" })
                        })
                        .px_1()
                        .py_0p5()
                        .rounded_sm()
                        .cursor_pointer()
                        .when(spoken, |paragraph| paragraph.bg(rgb(palette.selected)))
                        .hover(move |paragraph| paragraph.bg(rgb(palette.hover)))
                        .child(text)
                        .on_click(cx.listener(move |this, _, _, cx| this.play_from(start, cx))),
                )
                .into_any_element()
        });
        std::iter::once(header.into_any_element()).chain(paragraphs).collect()
    }
}

/// A paragraph's words, the query's matches highlighted (the one shown now, `current`, strongly).
fn highlighted(
    text: &str,
    query: Option<&str>,
    current: Option<&std::ops::Range<usize>>,
    palette: Palette,
) -> AnyElement {
    let ranges = query.map_or_else(Vec::new, |query| crate::recap::matched_ranges(text, query));
    if ranges.is_empty() {
        return div().text_sm().child(text.to_owned()).into_any_element();
    }
    let strong = HighlightStyle {
        background_color: Some(gpui::rgb(theme::SEARCH_MATCH).into()),
        color: Some(gpui::black()),
        ..HighlightStyle::default()
    };
    let other = HighlightStyle {
        background_color: Some(gpui::rgb(palette.selection).into()),
        ..HighlightStyle::default()
    };
    let highlights: Vec<_> = ranges
        .into_iter()
        .map(|range| {
            let mark = if current == Some(&range) { strong } else { other };
            (range, mark)
        })
        .collect();
    div()
        .text_sm()
        .child(StyledText::new(text.to_owned()).with_highlights(highlights))
        .into_any_element()
}

/// The left card: the recording, the Speakers chip and Teams' Copilot upsell for speaker insights.
fn recording_card(player: Option<Entity<VideoPlayer>>, palette: Palette) -> AnyElement {
    let speakers = div()
        .debug_selector(|| "recap-speakers".to_owned())
        .flex_none()
        .h(px(30.0))
        .px_3()
        .flex()
        .items_center()
        .rounded_full()
        .border_1()
        .border_color(rgb(palette.brand))
        .text_sm()
        .text_color(rgb(palette.brand))
        .child("Speakers");
    let upsell = div().flex().gap_4().child(illustration(Glyph::USERS, palette)).child(
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_x_1()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Upgrade to")
                    .child(div().text_color(rgb(palette.brand)).child("Microsoft 365 Copilot"))
                    .child("for AI speaker insights"),
            )
            .child(div().text_sm().child(
                "Switch to a Microsoft 365 Copilot plan for AI-generated meeting summaries with decisions, action \
                     items, and speaker details, organized into chapters so you can catch up faster.",
            ))
            .child(upgrade_button("recap-upgrade-speakers")),
    );
    card(palette)
        .debug_selector(|| "recap-recording".to_owned())
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .children(player)
        .child(
            div()
                .p_3()
                .flex()
                .flex_col()
                .gap_4()
                .child(div().flex().child(speakers))
                .child(upsell),
        )
        .into_any_element()
}

/// The AI summary pane: Teams' upsell for an account without Microsoft 365 Copilot.
fn summary_upsell(palette: Palette) -> AnyElement {
    let point = |text: &'static str| {
        div()
            .w_full()
            .flex()
            .gap_2()
            .text_sm()
            .child(
                div()
                    .flex_none()
                    .pt_0p5()
                    .child(Glyph::SPARKLES.icon().size(px(14.0)).text_color(rgb(palette.brand))),
            )
            .child(div().flex_1().min_w_0().child(text))
    };
    div()
        .debug_selector(|| "recap-summary-upsell".to_owned())
        .flex_1()
        .p_6()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_4()
        .child(illustration(Glyph::SPARKLES, palette))
        .child(
            div()
                .flex()
                .flex_wrap()
                .justify_center()
                .gap_1()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .child("Upgrade your")
                .child(div().text_color(rgb(palette.brand)).child("Copilot plan"))
                .child("for AI summaries"),
        )
        .child(
            div()
                .w_full()
                .max_w(px(420.0))
                .flex()
                .flex_col()
                .gap_2()
                .child(point("Transcribe and translate meetings in real time"))
                .child(point("Recap key points and tasks from discussions"))
                .child(point(
                    "Get AI answers that are informed by your work calendar, chats, files, and organizational data",
                )),
        )
        .child(upgrade_button("recap-upgrade-summary"))
        .into_any_element()
}

/// "Upgrade Copilot": Microsoft 365's Copilot offer, in the browser.
fn upgrade_button(id: &'static str) -> AnyElement {
    let button = Button::new(id)
        .label(UPGRADE_COPILOT)
        .outline()
        .on_click(|_, _, cx| cx.open_url(UPGRADE_LINK));
    div().flex().child(selected_by(id, button)).into_any_element()
}

/// An upsell's picture: its icon large in a tinted box.
fn illustration(glyph: Glyph, palette: Palette) -> AnyElement {
    div()
        .flex_none()
        .size(px(96.0))
        .rounded_lg()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgb(palette.selected))
        .text_color(rgb(palette.brand))
        .child(glyph.icon().size(px(44.0)))
        .into_any_element()
}

/// A file the chat shared, as a chip that opens it in the browser.
fn file_chip(index: usize, file: &SharedItem, palette: Palette) -> AnyElement {
    let url = file.url.clone();
    div()
        .id(SharedString::from(format!("recap-file-{index}")))
        .debug_selector(move || format!("recap-file-{index}"))
        .max_w(px(240.0))
        .h(px(32.0))
        .px_3()
        .flex()
        .items_center()
        .gap_2()
        .rounded_md()
        .border_1()
        .border_color(rgb(palette.border))
        .cursor_pointer()
        .hover(move |chip| chip.bg(rgb(palette.hover)))
        .child(Glyph::FILE.icon().size(px(14.0)))
        .child(div().text_sm().truncate().child(file.name.clone()))
        .on_click(move |_, _, cx| cx.open_url(&url))
        .into_any_element()
}

/// A line of the transcript that isn't anyone's words: an icon and muted words.
fn event_row(selector: &'static str, glyph: Glyph, text: String, palette: Palette) -> AnyElement {
    div()
        .debug_selector(move || selector.to_owned())
        .pt_3()
        .flex()
        .items_center()
        .gap_3()
        .text_xs()
        .text_color(rgb(palette.muted_foreground))
        .child(
            div()
                .w(px(24.0))
                .flex()
                .justify_center()
                .child(glyph.icon().size(px(14.0))),
        )
        .child(text)
        .into_any_element()
}

/// A raised, rounded box.
fn card(palette: Palette) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .rounded_lg()
        .border_1()
        .border_color(rgb(palette.border))
        .bg(rgb(palette.surface))
}

fn centred(child: impl IntoElement, palette: Palette) -> gpui::Div {
    div()
        .flex_1()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .text_color(rgb(palette.muted_foreground))
        .child(child)
}

fn spinner(palette: Palette) -> AnyElement {
    div()
        .debug_selector(|| "recap-loading".to_owned())
        .text_color(rgb(palette.brand))
        .child(Glyph::LOADING.icon().size(px(28.0)))
        .into_any_element()
}

/// An empty or failed state: its icon in a tinted circle and its words.
fn empty_state(glyph: Glyph, words: &'static str, palette: Palette) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap_3()
        .child(
            div()
                .size(px(56.0))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgb(palette.selected))
                .text_color(rgb(palette.brand))
                .child(glyph.icon().size(px(26.0))),
        )
        .child(
            div()
                .debug_selector(move || format!("recap-state {words}"))
                .text_color(rgb(palette.foreground))
                .child(words),
        )
}

/// A row of one of the recap's menus.
fn menu_row(selector: &'static str, glyph: Glyph, label: &'static str) -> PopupMenuItem {
    PopupMenuItem::element(move |_, _| {
        div()
            .debug_selector(move || selector.to_owned())
            .flex()
            .items_center()
            .gap_2()
            .child(glyph.icon().size(px(14.0)))
            .child(label)
    })
}
