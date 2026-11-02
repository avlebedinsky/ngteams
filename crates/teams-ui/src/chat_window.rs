//! Which of the "Chats" section's rows the sidebar shows, as Teams pages its recent chats
//! (`docs/protocol/chat.md`, "The chat list's order and its See more"). The rows themselves stay whole in the
//! [`crate::ChatList`]: the switcher, the search and the notifications reach every chat.
//!
//! - **The top rows**: the first [`FIRST_PAGE`], and each [`ChatWindow::see_more`] adds [`NEXT_PAGE`], for as long as
//!   rows are left. There is no way back but restarting, as in Teams; folding the section keeps what was shown.
//! - **A row keeps its place on screen**: a hidden chat that gets a new message comes in at the top and the rows shown
//!   stay shown, so the top rows grow by one rather than one dropping out, as Teams adds a row to its loaded page. A
//!   catch-up ([`ChatWindow::caught_up`]) is the exception: what it moved up doesn't add to them, or the first catch-up
//!   of a run would show every chat that moved while the app was closed.
//! - **A chat opened from below them** (the switcher, the search, a notification) is listed after them, in its own
//!   order, and stays there until See more or a new message brings it in with the top rows.

use std::collections::HashSet;

use crate::chat_list::ChatRow;

/// The rows shown at first: Teams' `recentChatsInitialFetchCountOverrideWithMeetingChatSectionEnabled`.
pub const FIRST_PAGE: usize = 20;
/// The rows each See more adds: Teams' `recentChatsSeeMoreFetchCountOverride`.
pub const NEXT_PAGE: usize = 25;

/// What is left below the rows shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeeMore {
    /// How many rows.
    pub hidden: usize,
    /// Whether one of them has something unread: See more is drawn bold, as Teams draws it.
    pub unread: bool,
}

/// The rows shown (see the module docs).
#[derive(Clone, Debug)]
pub struct ChatWindow {
    /// The top rows asked for: [`FIRST_PAGE`] and what See more added.
    asked: usize,
    /// The top rows shown, by chat.
    top: HashSet<String>,
    /// How many there are.
    count: usize,
    /// The chats opened from below them, and listed after them.
    opened: HashSet<String>,
}

impl Default for ChatWindow {
    fn default() -> Self {
        Self {
            asked: FIRST_PAGE,
            top: HashSet::new(),
            count: 0,
            opened: HashSet::new(),
        }
    }
}

impl ChatWindow {
    /// The window over `rows` as they are now (the latest activity first), with the chat `selected` open.
    pub fn settle(&mut self, rows: &[ChatRow], selected: Option<&str>) {
        let reached = rows
            .iter()
            .rposition(|row| self.top.contains(&row.id))
            .map_or(0, |at| at + 1);
        self.count = reached.max(self.asked).min(rows.len());
        let (top, below) = rows.split_at(self.count);
        self.top = top.iter().map(|row| row.id.clone()).collect();
        self.opened.retain(|id| below.iter().any(|row| &row.id == id));
        if let Some(selected) = selected.filter(|id| below.iter().any(|row| row.id == *id)) {
            self.opened.insert(selected.to_owned());
        }
    }

    /// See more: [`NEXT_PAGE`] more top rows.
    pub fn see_more(&mut self, rows: &[ChatRow], selected: Option<&str>) {
        self.asked = self.count + NEXT_PAGE;
        self.settle(rows, selected);
    }

    /// A catch-up ended: the next [`Self::settle`] shows only what was asked for (see the module docs).
    pub fn caught_up(&mut self) {
        self.top.clear();
    }

    /// The rows shown, as indices of `rows`, in their order.
    #[must_use]
    pub fn shown(&self, rows: &[ChatRow]) -> Vec<usize> {
        let opened = rows
            .iter()
            .enumerate()
            .skip(self.count)
            .filter(|(_, row)| self.opened.contains(&row.id))
            .map(|(at, _)| at);
        (0..self.count.min(rows.len())).chain(opened).collect()
    }

    /// What is left below the rows shown, when anything is: what See more offers.
    #[must_use]
    pub fn see_more_offered(&self, rows: &[ChatRow]) -> Option<SeeMore> {
        let hidden: Vec<&ChatRow> = rows
            .iter()
            .skip(self.count)
            .filter(|row| !self.opened.contains(&row.id))
            .collect();
        (!hidden.is_empty()).then(|| SeeMore {
            hidden: hidden.len(),
            unread: hidden.iter().any(|row| row.unread > 0),
        })
    }
}
