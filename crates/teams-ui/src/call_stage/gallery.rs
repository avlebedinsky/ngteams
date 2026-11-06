//! The stage of a call that is up, as Teams lays it out: the gallery (a grid of near-square tiles), the speaker view
//! (the one who speaks, or the one pinned, large over a strip of the others), the self view floating at its bottom
//! right unless View hid it, and each tile's own menu (Pin for me, Unpin). While the far end shares its screen, the
//! share fills the stage with the people's tiles (the account's own among them) in a strip on its right, as Teams
//! shows a share; View's Focus on content shows the share alone.
use super::*;

/// How the stage lays its tiles out: View's Gallery or Speaker, or (while a share shows) Focus on content.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Layout {
    #[default]
    Gallery,
    Speaker,
    /// The share alone, the people's strip put away (laid out as the gallery while nothing is shared).
    Focus,
}

impl Layout {
    /// The trace's (and the test selectors') name for it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Gallery => "gallery",
            Self::Speaker => "speaker",
            Self::Focus => "focus",
        }
    }
}

/// The height of the speaker view's strip of the others, px.
const STRIP_HEIGHT: f32 = 112.0;
/// The width of a tile in the speaker view's strip, px.
const STRIP_TILE: f32 = 184.0;
/// The width of the people's strip beside a share, and the height of a tile in it, px (Teams' 16:9 tiles).
const SHARE_STRIP: (f32, f32) = (236.0, 132.0);

impl CallStage {
    /// The stage of the call up: who rings while nobody is in it, else the tiles as View lays them out, with the self
    /// view over them.
    pub(super) fn stage(
        &mut self,
        active: &ActiveCall,
        title: &SharedString,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // waiting in the lobby, the account sees nobody: only that someone should let it in
        // waiting in the lobby, holding the call or held by the far end, the account sees nobody's tile
        let tiles = if active.status == CallStatus::Lobby || active.held.is_some() || active.held_by_far_end {
            Vec::new()
        } else {
            self.tiles_of(active)
        };
        let call = active.id.get();
        let lobby = crate::call_lobby::card(active, palette, cx);
        // the far end's share fills the stage, the people beside it (the account's own tile among them)
        let shared = active.remote_share && active.held.is_none() && active.status != CallStatus::Lobby;
        let body = if shared {
            self.share_stage(active, tiles, title, palette, cx)
        } else if tiles.is_empty() {
            let waiting = active.kind == CallKind::Meeting && active.status == CallStatus::Connected;
            if let Some(since) = active.held {
                // Teams' held call: who it is with, "On hold" and how long, no rings
                let lasted = calls::duration_text(since.elapsed().as_secs());
                calling(title, &format!("On hold {lasted}"), palette, Vec::new())
            } else if active.held_by_far_end {
                // Teams' held party: who holds it over "On hold", no duration, no rings
                div()
                    .debug_selector(|| "call-held-by-far-end".to_owned())
                    .size_full()
                    .child(calling(title, "On hold", palette, Vec::new()))
                    .into_any_element()
            } else {
                let state = if waiting {
                    "Waiting for others to join".to_owned()
                } else {
                    calls::status_text(active)
                };
                let rings = crate::motion::ripple("call-calling", palette.accent, BIG_AVATAR_PX, cx);
                calling(title, &state, palette, rings)
            }
        } else {
            let selector = if active.view.participants.is_empty() {
                "call-people"
            } else {
                "call-grid"
            };
            let focus = self.focus_of(&tiles);
            let menus = tiles.len() > 1;
            let mut drawn: Vec<AnyElement> = tiles
                .into_iter()
                .map(|tile| {
                    let video = tile
                        .video
                        .filter(|_| !self.others_off)
                        .and_then(|line| self.tiles.as_mut()?.fill(line, call));
                    let pinned = tile.mri.is_some() && tile.mri == self.pinned;
                    tile_view(tile, video, (pinned, menus), palette, cx)
                })
                .collect();
            match focus {
                Some(index) if drawn.len() > 1 => {
                    let main = drawn.remove(index);
                    speaker(selector, main, drawn, !self.self_hidden)
                }
                _ => grid(selector, drawn),
            }
        };
        let own = if self.self_hidden || shared {
            None
        } else {
            let video = self.tiles.as_mut().and_then(|tiles| tiles.fill(Tile::Own, call));
            Some(self_view(active, video, true, palette, cx))
        };
        div()
            .debug_selector(|| "call-stage".to_owned())
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .p_3()
            .bg(rgb(palette.background))
            .child(body)
            .children(own)
            .children(self.reactions_shown(active, palette, cx))
            .children(lobby)
            .into_any_element()
    }

    /// The far end's share large, its sharer named at its bottom left (its picture fitted in, or that it is coming),
    /// and the people's tiles in a strip on its right with the account's own last (unless View hid it); Focus on
    /// content shows the share alone.
    fn share_stage(
        &mut self,
        active: &ActiveCall,
        tiles: Vec<StageTile>,
        title: &SharedString,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let call = active.id.get();
        // who the roster says shares (a group call's), else the far end
        let sharing = active
            .view
            .participants
            .iter()
            .find(|p| p.sharing)
            .map(|p| p.mri.as_str());
        let sharer = tiles
            .iter()
            .find(|tile| sharing.is_some() && tile.mri.as_deref() == sharing)
            .or(tiles.first())
            .map_or_else(|| title.clone(), |tile| tile.name.clone());
        let picture = self.tiles.as_mut().and_then(|tiles| tiles.fill(Tile::Share, call));
        let content = share_content(picture, sharer, palette);
        if self.layout == Layout::Focus {
            return div()
                .debug_selector(|| "call-share-focus".to_owned())
                .size_full()
                .child(content)
                .into_any_element();
        }
        let mut strip: Vec<AnyElement> = tiles
            .into_iter()
            .map(|tile| {
                let video = tile
                    .video
                    .filter(|_| !self.others_off)
                    .and_then(|line| self.tiles.as_mut()?.fill(line, call));
                tile_view(tile, video, (false, false), palette, cx)
            })
            .collect();
        if !self.self_hidden {
            let video = self.tiles.as_mut().and_then(|tiles| tiles.fill(Tile::Own, call));
            strip.push(self_view(active, video, false, palette, cx));
        }
        div()
            .debug_selector(|| "call-share-stage".to_owned())
            .size_full()
            .flex()
            .gap_2()
            .child(div().flex_1().min_w_0().h_full().child(content))
            .child(
                div()
                    .debug_selector(|| "call-share-strip".to_owned())
                    .w(px(SHARE_STRIP.0))
                    .flex_none()
                    .h_full()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .gap_2()
                    .children(
                        strip
                            .into_iter()
                            .map(|tile| div().w_full().h(px(SHARE_STRIP.1)).flex_none().child(tile)),
                    ),
            )
            .into_any_element()
    }

    /// Which tile the stage shows large, if any: the one pinned; in the speaker view the one who speaks, else who
    /// spoke last, else the first.
    fn focus_of(&self, tiles: &[StageTile]) -> Option<usize> {
        let at = |mri: &Option<String>| {
            mri.as_ref()
                .and_then(|mri| tiles.iter().position(|tile| tile.mri.as_ref() == Some(mri)))
        };
        if let Some(index) = at(&self.pinned) {
            return Some(index);
        }
        if self.layout != Layout::Speaker {
            return None;
        }
        tiles
            .iter()
            .position(|tile| tile.speaking)
            .or_else(|| at(&self.last_speaker))
            .or(Some(0))
    }

    /// The stage's tiles: the roster's participants once it told who is in the call (the grid, traced when it
    /// changed); before that, once the media connected, the people the call is with (a 1:1 call's far end on line
    /// 0); none while it rings.
    pub(super) fn tiles_of(&mut self, active: &ActiveCall) -> Vec<StageTile> {
        let view = &active.view;
        if !view.participants.is_empty() {
            let people = self.people.of(active.id);
            let mut tiles: Vec<StageTile> = view
                .participants
                .iter()
                .enumerate()
                .map(|(index, participant)| StageTile {
                    selector: format!("call-participant-{index}"),
                    mri: Some(participant.mri.clone()),
                    name: participant_name(participant, people),
                    speaking: participant.speaking,
                    muted: participant.muted,
                    hand: participant.hand_raised,
                    video_on: participant.video_on,
                    video: participant.line.map(Tile::Line),
                })
                .collect();
            if let Some(speaking) = tiles.iter().find(|tile| tile.speaking) {
                self.last_speaker.clone_from(&speaking.mri);
            }
            if self.videos_first {
                // stable: the cameras on lead, each side in the roster's order
                tiles.sort_by_key(|tile| !tile.video_on);
            }
            self.trace_grid(active.id, view);
            return tiles;
        }
        if !matches!(active.status, CallStatus::Connected | CallStatus::Reconnecting) {
            return Vec::new();
        }
        let people = self.people.of(active.id);
        let one = people.len() == 1;
        people
            .iter()
            .enumerate()
            .map(|(index, (mri, name))| StageTile {
                selector: format!("call-person-{index}"),
                mri: Some(mri.clone()),
                name: name.clone(),
                speaking: false,
                muted: false,
                hand: false,
                video_on: false,
                video: one.then_some(Tile::REMOTE),
            })
            .collect()
    }

    /// Pins the participant `mri` for the account alone (Teams' Pin for me), or unpins whoever is pinned.
    pub fn pin(&mut self, mri: Option<String>, cx: &mut Context<Self>) {
        self.pinned = mri;
        self.trace.record(
            &TraceLine::new("call-window", "pinned")
                .call(self.call.get())
                .kind(if self.pinned.is_some() { "pinned" } else { "unpinned" }),
        );
        cx.notify();
    }

    /// Lays the stage out as View's `layout`.
    pub fn set_layout(&mut self, layout: Layout, cx: &mut Context<Self>) {
        if self.layout != layout {
            self.layout = layout;
            self.trace.record(
                &TraceLine::new("call-window", "layout")
                    .call(self.call.get())
                    .kind(layout.name()),
            );
        }
        cx.notify();
    }

    /// The stage's layout, as View chose it.
    #[must_use]
    pub fn layout(&self) -> Layout {
        self.layout
    }

    /// Hides the self view, or shows it again (View's Hide for me).
    pub fn hide_self_view(&mut self, hidden: bool, cx: &mut Context<Self>) {
        self.self_hidden = hidden;
        cx.notify();
    }

    /// Leads with the tiles whose camera is on, or keeps the roster's order (View's Show videos first).
    pub fn show_videos_first(&mut self, first: bool, cx: &mut Context<Self>) {
        self.videos_first = first;
        cx.notify();
    }

    /// Shows the others' initials in place of their video, or their video again (View's Turn off other videos), as
    /// Teams does to spare a slow connection or a busy screen. Only what this window draws changes.
    pub fn turn_off_other_videos(&mut self, off: bool, cx: &mut Context<Self>) {
        self.others_off = off;
        self.trace.record(
            &TraceLine::new("call-window", "other-videos")
                .call(self.call.get())
                .kind(if off { "off" } else { "on" }),
        );
        cx.notify();
    }

    /// Who has a hand up, kept in the order they raised it: a hand lowered leaves the line, one raised joins its end.
    pub(super) fn order_hands(&mut self, active: &ActiveCall) {
        let me = self.core.me().to_owned();
        let up: Vec<&str> = active
            .view
            .participants
            .iter()
            .filter(|participant| participant.hand_raised)
            .map(|participant| participant.mri.as_str())
            .chain(active.hand_raised.then_some(me.as_str()))
            .collect();
        self.hands.retain(|mri| up.contains(&mri.as_str()));
        for mri in up {
            if !self.hands.iter().any(|raised| raised == mri) {
                self.hands.push(mri.to_owned());
            }
        }
    }

    /// Where `mri`'s hand stands in the line of hands up, from 1; none while it is down.
    pub(super) fn hand_rank(&self, mri: &str) -> Option<usize> {
        self.hands
            .iter()
            .position(|raised| raised == mri)
            .map(|index| index + 1)
    }
}

/// The tiles in a grid of near-square rows, each tile as wide as a column, a short last row centred: Teams' gallery.
fn grid(selector: &'static str, tiles: Vec<AnyElement>) -> AnyElement {
    let count = tiles.len();
    let columns = (1..=count).find(|columns| columns * columns >= count).unwrap_or(1);
    let width = 1.0 / f32::from(u16::try_from(columns).unwrap_or(1));
    let mut rows: Vec<Vec<AnyElement>> = Vec::new();
    for tile in tiles {
        match rows.last_mut() {
            Some(row) if row.len() < columns => row.push(tile),
            _ => rows.push(vec![tile]),
        }
    }
    div()
        .debug_selector(move || selector.to_owned())
        .size_full()
        .flex()
        .flex_col()
        .children(rows.into_iter().map(|row| {
            div().flex_1().min_h_0().w_full().flex().justify_center().children(
                row.into_iter()
                    .map(|tile| div().w(relative(width)).h_full().p_1().child(tile)),
            )
        }))
        .into_any_element()
}

/// The speaker view: `main` large, the others in a strip under it (clear of the self view while it shows).
fn speaker(selector: &'static str, main: AnyElement, others: Vec<AnyElement>, self_shown: bool) -> AnyElement {
    div()
        .debug_selector(move || selector.to_owned())
        .size_full()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .debug_selector(|| "call-speaker-main".to_owned())
                .flex_1()
                .min_h_0()
                .w_full()
                .p_1()
                .child(main),
        )
        .child(
            div()
                .debug_selector(|| "call-speaker-strip".to_owned())
                .h(px(STRIP_HEIGHT))
                .flex_none()
                .w_full()
                .flex()
                .justify_center()
                .gap_2()
                .overflow_hidden()
                .when(self_shown, |strip| strip.pr(px(SELF_VIEW.0 + 16.0)))
                .children(
                    others
                        .into_iter()
                        .map(|tile| div().w(px(STRIP_TILE)).flex_none().h_full().child(tile)),
                ),
        )
        .into_any_element()
}

/// One tile: its video when a line shows it, else its initials; its name with its microphone muted and a raised
/// hand; a border in the brand colour while it speaks (Teams' gold while its hand is up); its menu on hover when the
/// stage has more than one (`menus`), its pin while `pinned`.
fn tile_view(
    tile: StageTile,
    video: Option<AnyElement>,
    (pinned, menus): (bool, bool),
    palette: Palette,
    cx: &mut Context<CallStage>,
) -> AnyElement {
    let StageTile {
        selector,
        mri,
        name,
        speaking,
        muted,
        hand,
        ..
    } = tile;
    let face = video.unwrap_or_else(|| {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(people::avatar(Some(&name), None, BIG_AVATAR))
            .into_any_element()
    });
    let glow_id = SharedString::from(format!("{selector}-speaking"));
    let group = SharedString::from(format!("{selector}-tile"));
    let menu = mri
        .filter(|_| menus)
        .map(|mri| tile_menu(&selector, mri, pinned, &group, palette, cx));
    let border = if hand {
        HAND_COLOUR
    } else if speaking {
        palette.brand
    } else {
        palette.rail
    };
    let tile = div()
        .debug_selector(move || selector.clone())
        .group(group)
        .relative()
        .size_full()
        .rounded_lg()
        .overflow_hidden()
        // a dark tile with a hint of the brand colour from its top
        .bg(theme::gradient(
            theme::mix(palette.rail, palette.brand, 0.2),
            palette.rail,
            160.0,
        ))
        .border_2()
        .border_color(rgb(border))
        .child(face)
        .child(name_tag(name, (speaking, muted, hand, pinned), palette))
        .children(menu);
    // while its person speaks, a glow in the brand colour swells and settles round it
    crate::motion::breathe(tile, glow_id, speaking, palette, cx)
}

/// Teams' raised hand's gold, px colour.
pub(super) const HAND_COLOUR: u32 = 0x00f2_c94c;

/// A tile's "More options" at its top right, shown while the pointer is over the tile: Pin for me, or Unpin.
fn tile_menu(
    selector: &str,
    mri: String,
    pinned: bool,
    group: &SharedString,
    palette: Palette,
    cx: &mut Context<CallStage>,
) -> AnyElement {
    let id = SharedString::from(format!("{selector}-more"));
    let stage = cx.weak_entity();
    let item = SharedString::from(format!("{selector}-pin"));
    let group = group.clone();
    div()
        .absolute()
        .top_2()
        .right_2()
        .invisible()
        .group_hover(group, gpui::Styled::visible)
        .child(selected_by(
            id.to_string(),
            Button::new(id)
                .ghost()
                .xsmall()
                .icon(Glyph::MORE.icon())
                .bg(rgb(palette.surface))
                .tooltip("More options")
                .dropdown_menu_with_anchor(Anchor::TopRight, move |menu: PopupMenu, _, _| {
                    let (stage, mri, item) = (stage.clone(), mri.clone(), item.clone());
                    let (glyph, label) = if pinned {
                        (Glyph::PIN_OFF, "Unpin")
                    } else {
                        (Glyph::PIN, "Pin for me")
                    };
                    menu.min_w(px(180.0)).item(
                        PopupMenuItem::element(move |_, _| {
                            let item = item.clone();
                            div()
                                .debug_selector(move || item.to_string())
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(glyph.icon().size(px(14.0)))
                                .child(label)
                        })
                        .on_click(move |_, _, cx| {
                            let mri = (!pinned).then(|| mri.clone());
                            let _ = stage.update(cx, |stage, cx| stage.pin(mri, cx));
                        }),
                    )
                }),
        ))
        .into_any_element()
}

/// A tile's name at its bottom left: a pin while pinned, a raised hand, its name, a speaker while it speaks and its
/// microphone while muted.
fn name_tag(
    name: SharedString,
    (speaking, muted, hand, pinned): (bool, bool, bool, bool),
    palette: Palette,
) -> gpui::Div {
    div()
        .absolute()
        .bottom_2()
        .left_2()
        .max_w(relative(0.9))
        .px_2()
        .py_0p5()
        .flex()
        .items_center()
        .gap_1()
        .rounded_md()
        .bg(rgb(palette.surface))
        .text_xs()
        .when(pinned, |tag| tag.child(Glyph::PIN.icon().size(px(12.0))))
        .when(hand, |tag| {
            tag.child(Glyph::HAND.icon().size(px(12.0)).text_color(rgb(HAND_COLOUR)))
        })
        .child(div().truncate().child(name))
        .when(speaking && !muted, |tag| {
            tag.child(Glyph::VOLUME.icon().size(px(12.0)).text_color(rgb(palette.brand)))
        })
        .when(muted, |tag| {
            tag.child(
                Glyph::MIC_OFF
                    .icon()
                    .size(px(12.0))
                    .text_color(rgb(palette.muted_foreground)),
            )
        })
}

/// The share as the stage shows it: its picture fitted in (on the stage's own ground, as Teams letterboxes it), or
/// while its first frame is coming the sharer's screen's mark and who shares; the sharer named at its bottom left.
fn share_content(picture: Option<AnyElement>, sharer: SharedString, palette: Palette) -> AnyElement {
    let face = picture.unwrap_or_else(|| {
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .text_color(rgb(palette.muted_foreground))
            .child(Glyph::MONITOR.icon().size(px(40.0)))
            .child(div().text_sm().child(format!("{sharer} is sharing")))
            .into_any_element()
    });
    div()
        .debug_selector(|| "call-share".to_owned())
        .relative()
        .size_full()
        .overflow_hidden()
        .child(face)
        .child(name_tag(sharer, (false, false, false, false), palette))
        .into_any_element()
}

/// The account's own tile: floating at the stage's bottom right (`floating`), else filling its place in a strip; its
/// self view while the camera sends, else its mark; a border while it speaks; its microphone while muted and its hand
/// while raised.
fn self_view(active: &ActiveCall, video: Option<AnyElement>, floating: bool, palette: Palette, cx: &App) -> AnyElement {
    let speaking = active.view.speaking && !active.muted;
    let face = video.unwrap_or_else(|| {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_color(rgb(palette.muted_foreground))
            .child(Glyph::USER.icon().size(px(32.0)))
            .into_any_element()
    });
    let border = if active.hand_raised {
        HAND_COLOUR
    } else if speaking {
        palette.brand
    } else {
        palette.border
    };
    let own = div()
        .debug_selector(|| "call-participant-self".to_owned())
        .when(floating, |own| {
            own.absolute()
                .bottom_5()
                .right_5()
                .w(px(SELF_VIEW.0))
                .h(px(SELF_VIEW.1))
        })
        .when(!floating, gpui::Styled::size_full)
        .rounded_lg()
        .overflow_hidden()
        .bg(rgb(palette.surface))
        .border_2()
        .border_color(rgb(border))
        .when(!speaking && floating, |own| {
            own.shadow(theme::glow(palette, 0x0000_0000, 0.3, 18.0, 6.0))
        })
        .child(face)
        .child(name_tag(
            SharedString::from("You"),
            (speaking, active.muted, active.hand_raised, false),
            palette,
        ));
    crate::motion::breathe(own, "call-self-speaking", speaking, palette, cx)
}

impl CallStage {
    /// The live reactions sent lately, floating up from the stage's bottom left as Teams floats them: each its picture
    /// and who sent it ("You" for the account's own), gone after [`calls::REACTION_SHOWN`].
    pub(super) fn reactions_shown(&self, active: &ActiveCall, palette: Palette, cx: &App) -> Option<AnyElement> {
        let now = std::time::Instant::now();
        let shown: Vec<&calls::LiveReaction> = active
            .reactions
            .iter()
            .filter(|reaction| now.duration_since(reaction.at) < calls::REACTION_SHOWN)
            .collect();
        if shown.is_empty() {
            return None;
        }
        let me = self.core.me().to_owned();
        let people = self.people.of(active.id);
        let floats: Vec<AnyElement> = shown
            .into_iter()
            .enumerate()
            .map(|(index, reaction)| {
                let key = toolbar::REACTIONS
                    .iter()
                    .find(|(name, _, _)| *name == reaction.reaction)
                    .map_or(reaction.reaction.as_str(), |(_, key, _)| key);
                let who = reaction_sender(reaction, &me, people);
                let float = div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_0p5()
                    .child(crate::reactions::emoji(key, px(40.0)))
                    .child(
                        div()
                            .max_w(px(120.0))
                            .px_1p5()
                            .rounded_md()
                            .bg(rgb(palette.surface))
                            .text_xs()
                            .truncate()
                            .child(who),
                    );
                // each one rises into place as it comes, as Teams' float up
                let id = SharedString::from(format!("call-reaction-{index}-{}", reaction.reaction));
                crate::motion::rise(float, id, 24.0, cx)
            })
            .collect();
        Some(
            div()
                .debug_selector(|| "call-reactions".to_owned())
                .absolute()
                .bottom(px(56.0))
                .left_5()
                .flex()
                .items_end()
                .gap_2()
                .children(floats)
                .into_any_element(),
        )
    }

    /// While a live reaction shows, the stage is drawn again as the next one is due to go.
    pub(super) fn float_reactions(&mut self, active: &ActiveCall, cx: &mut Context<Self>) {
        let now = std::time::Instant::now();
        let Some(due) = active
            .reactions
            .iter()
            .map(|reaction| (reaction.at + calls::REACTION_SHOWN).saturating_duration_since(now))
            .filter(|due| !due.is_zero())
            .min()
        else {
            self.reacting = None;
            return;
        };
        self.reacting = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
            cx.background_executor().timer(due).await;
            // a stage that is gone has nothing left to draw
            let _ = this.update(cx, |this, cx| {
                if let Some(active) = this.live(cx) {
                    this.float_reactions(&active, cx);
                }
                cx.notify();
            });
        }));
    }
}

/// Who sent `reaction`: "You" for the account's own (`me`), else the name it came with, the call's people's name for
/// them, or "Someone".
fn reaction_sender(reaction: &calls::LiveReaction, me: &str, people: &[(String, SharedString)]) -> SharedString {
    if reaction.from.as_str() == me {
        return SharedString::from("You");
    }
    reaction
        .name
        .clone()
        .map(SharedString::from)
        .or_else(|| {
            people
                .iter()
                .find(|(mri, _)| mri == reaction.from.as_str())
                .map(|(_, name)| name.clone())
        })
        .unwrap_or_else(|| SharedString::from(SOMEONE))
}

/// While the call reconnects, a bar over the stage saying so, as Teams says it; nothing while its connection is fine.
pub(super) fn connection_banner(active: &ActiveCall, palette: Palette) -> Option<gpui::Div> {
    (active.status == CallStatus::Reconnecting).then(|| {
        div()
            .debug_selector(|| "call-reconnecting".to_owned())
            .flex_none()
            .px_4()
            .py_1p5()
            .flex()
            .items_center()
            .gap_2()
            .text_sm()
            .bg(rgb(palette.surface))
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(Glyph::WIFI.icon().size(px(14.0)).text_color(rgb(theme::DANGER)))
            .child("Reconnecting… Your call will continue once your connection is back.")
    })
}
