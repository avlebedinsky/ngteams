//! The faces a call shows of its people, as Teams shows them: each one's profile picture, read from the engine the way
//! the chat list and the profile card read it ([`Core::person`]: the stored picture, a lookup started in the background
//! when it is missing or old), else the initials of their name. A tile without video centres its face, round and
//! sized to the tile as Teams sizes it ([`face_side`]): large in a 1:1 call's stage, smaller in a grid, the strips and
//! the self view.
//!
//! The account, the people the call is with, the roster's participants, those invited and those waiting in the lobby
//! are asked about once each; someone whose profile changed later (`Event::Profile`, as the chats' model counts it) is
//! asked again. A face shows initials while its picture is being read.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use super::*;
use crate::people::{Picture, Shown};

/// The side of a tile's face before its tile was measured, px: the kit's large avatar's.
const UNMEASURED: f32 = BIG_AVATAR_PX;
/// The smallest and the largest face a tile shows, px.
const FACE_RANGE: (f32, f32) = (32.0, 200.0);

/// The pictures of a call's people, by MRI, and the tiles' sides as last laid out.
#[derive(Default)]
pub(super) struct Faces {
    /// Per MRI: the picture read, `None` when they have none.
    read: HashMap<String, Option<Picture>>,
    /// Asked about and not answered yet.
    asking: HashSet<String>,
    /// The chats' revision whose profile changes were last looked at.
    seen: u64,
    /// Per tile (by its selector): the side of its face, from the tile's size as last laid out.
    sides: Rc<RefCell<HashMap<String, f32>>>,
}

impl Faces {
    /// `mri`'s picture, once read; none while it is read, or when they have none.
    pub(super) fn picture(&self, mri: &str) -> Option<&Picture> {
        self.read.get(mri).and_then(Option::as_ref)
    }

    /// Of `wanted`, those not asked about yet, now marked asked.
    fn unasked<'a>(&mut self, wanted: impl IntoIterator<Item = &'a str>) -> Vec<String> {
        let mut ask = Vec::new();
        for mri in wanted {
            if !self.read.contains_key(mri) && self.asking.insert(mri.to_owned()) {
                ask.push(mri.to_owned());
            }
        }
        ask
    }

    /// The side of the face tile `selector` shows, as its size was last laid out.
    fn side(&self, selector: &str) -> f32 {
        self.sides.borrow().get(selector).copied().unwrap_or(UNMEASURED)
    }
}

/// The side of the face a tile of these sides shows, px: Teams' grows with the tile's shorter side, from about half of
/// a small tile's (68 px in its 134 px self view) to a fifth of a large one's (200 px in a 745 px stage), and no larger;
/// never more than half the shorter side, so a short strip's tile keeps its name tag clear.
#[must_use]
pub(super) fn face_side(width: f32, height: f32) -> f32 {
    let shorter = width.min(height);
    (0.216 * shorter + 39.0)
        .min(shorter * 0.5)
        .clamp(FACE_RANGE.0, FACE_RANGE.1)
        .round()
}

/// Everyone `active` shows a face of: the account, the people it is with, the roster's participants, who was invited
/// and who waits in the lobby.
fn shown_in<'a>(active: &'a ActiveCall, me: &'a str, people: &'a [(String, SharedString)]) -> Vec<&'a str> {
    let mut mris = vec![me];
    mris.extend(people.iter().map(|(mri, _)| mri.as_str()));
    mris.extend(
        active
            .view
            .participants
            .iter()
            .map(|participant| participant.mri.as_str()),
    );
    mris.extend(active.invited.iter().map(|invited| invited.mri.as_str()));
    mris.extend(active.lobby.iter().map(|person| person.mri.as_str()));
    mris
}

impl CallStage {
    /// Asks the engine about the pictures of everyone the call shows who wasn't asked about yet.
    pub(super) fn ask_faces(&mut self, cx: &mut Context<Self>) {
        let Some(active) = self.live(cx) else { return };
        let me = self.core.me().to_owned();
        let wanted = shown_in(&active, &me, self.people.of(active.id));
        let ask = self.faces.unasked(wanted);
        self.read_faces(ask, cx);
    }

    /// Someone's profile may have changed (their picture read in the background): those already read are asked again.
    pub(super) fn profiles_changed(&mut self, chats: &Entity<crate::models::Chats>, cx: &mut Context<Self>) {
        let chats = chats.read(cx);
        let since = self.faces.seen;
        self.faces.seen = chats.revision();
        let changed: Vec<String> = self
            .faces
            .read
            .keys()
            .filter(|mri| chats.named_at(mri) > since && !self.faces.asking.contains(*mri))
            .cloned()
            .collect();
        for mri in &changed {
            self.faces.asking.insert(mri.clone());
        }
        self.read_faces(changed, cx);
    }

    /// Reads the pictures of `asked`; each is drawn once read (a picture read before stays until then).
    fn read_faces(&mut self, asked: Vec<String>, cx: &mut Context<Self>) {
        if asked.is_empty() {
            return;
        }
        let core = Arc::clone(&self.core);
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let mut found = Vec::with_capacity(asked.len());
            for mri in asked {
                let picture = core
                    .person(mri.clone())
                    .await
                    .ok()
                    .and_then(|person| Shown::of(person).picture);
                found.push((mri, picture));
            }
            // a stage that is gone shows nobody
            let _ = this.update(cx, |stage, cx| {
                for (mri, picture) in found {
                    stage.faces.asking.remove(&mri);
                    stage.faces.read.insert(mri, picture);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The face a tile without video centres: `name`'s picture when `mri` has one, else the initials of `name` (none:
    /// the account's mark), round and sized to the tile (`selector`'s), measured as it is laid out.
    pub(super) fn tile_face(
        &self,
        selector: &str,
        (mri, name): (Option<&str>, Option<&SharedString>),
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let side = self.faces.side(selector);
        let picture = mri.and_then(|mri| self.faces.picture(mri));
        let (shown, face) = match (picture, name) {
            (Some(_), _) | (None, Some(_)) => {
                let face = people::avatar(name, picture, Size::Size(px(side)))
                    // the kit sizes custom initials to a box of half the side: they are centred in it at Teams' size
                    .text_size(px(side * 0.4))
                    .line_height(px(side * 0.5))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_center()
                    .whitespace_nowrap();
                (
                    if picture.is_some() { "picture" } else { "initials" },
                    face.into_any_element(),
                )
            }
            (None, None) => (
                "mark",
                div()
                    .text_color(rgb(palette.muted_foreground))
                    .child(Glyph::USER.icon().size(px(32.0)))
                    .into_any_element(),
            ),
        };
        let selector = selector.to_owned();
        let face_selector = format!("{selector}-{shown}");
        div()
            .relative()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(self.measure_tile(selector, cx))
            .child(div().debug_selector(move || face_selector.clone()).child(face))
            .into_any_element()
    }

    /// An empty element over a tile that keeps the side of its face as the tile is laid out, drawing the stage again
    /// when it changed.
    fn measure_tile(&self, selector: String, cx: &mut Context<Self>) -> AnyElement {
        let sides = Rc::clone(&self.faces.sides);
        let stage = cx.weak_entity();
        gpui::canvas(
            move |bounds, window, _| {
                let side = face_side(f32::from(bounds.size.width), f32::from(bounds.size.height));
                if sides.borrow_mut().insert(selector, side) != Some(side) {
                    // the face takes its side on the next frame
                    window.on_next_frame(move |_, cx| {
                        let _ = stage.update(cx, |_, cx| cx.notify());
                    });
                }
            },
            |_, (), _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .into_any_element()
    }
}

/// A small face (the People pane's, the lobby's): `mri`'s picture once read, else the initials of `name`.
pub(super) fn small_face(faces: &Faces, mri: &str, name: &SharedString) -> gpui_component::avatar::Avatar {
    people::avatar(Some(name), faces.picture(mri), Size::Small)
}

#[cfg(test)]
mod tests {
    use super::face_side;

    #[test]
    fn a_face_grows_with_its_tile_as_teams_does_and_no_larger() {
        // Teams' self view (240 x 134) and its 1:1 stage (1360 x 745)
        assert!((face_side(240.0, 134.0) - 68.0).abs() <= 1.0);
        assert!((face_side(1360.0, 745.0) - 200.0).abs() <= 1.0);
        // the speaker view's strip, a 2 x 2 grid, a huge window, a tile not laid out yet
        assert!((face_side(184.0, 112.0) - 56.0).abs() <= 1.0);
        assert!(face_side(640.0, 360.0) < face_side(1360.0, 745.0));
        assert!((face_side(3000.0, 2000.0) - 200.0).abs() < f32::EPSILON);
        assert!((face_side(0.0, 0.0) - 32.0).abs() < f32::EPSILON);
    }
}
