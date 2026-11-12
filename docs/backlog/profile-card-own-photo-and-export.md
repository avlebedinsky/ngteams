---
worth: maybe
where: crates/teams-ui/src/profile_card/draw.rs
added: 2026-11-11
---
# Profile card: the account's own card's photo prompt and "Export data"

On the account's own card Teams shows "Help others recognize you by adding your photo." with "Upload a photo" when the
account has no picture, and a "More options" (…) menu with "Export data". The app's own card (`crate::profile_card`)
has neither: uploading a profile photo is a write to the directory nobody captured yet (Graph's photo upload, or
Teams' own), and "Export data" is the persona card's export of what it shows, whose request wasn't captured either.
Capture both, held back, with a lab account before building them.
