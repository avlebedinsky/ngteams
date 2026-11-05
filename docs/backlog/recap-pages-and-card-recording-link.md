---
worth: yes
where: crates/teams-proto/src/recap.rs, crates/teams-ui/src/recap_view.rs, crates/teams-proto/src/chat/event.rs
added: 2026-11-05
---
# a meeting's recap: MCPS's next page, and whether a recording card's link is MCPS's location

Two things the recap (`docs/protocol/recap.md`) takes on trust because no capture showed otherwise:

- **Paging.** MCPS answers `contents/?threadId=…` with `paginationInfo: {hasNextPage}`. `MeetingContents::read_answer`
  reads `resources` alone and no next page is asked, so a meeting chat whose contents span more than one page (a long
  recurring meeting) would show only the first page's parts, its "Part n" numbering and session line from that set.
  Every captured answer had `hasNextPage: false`; neither the page size nor how the next page is asked (a token, a
  `skip`) was seen.
- **"View recap" on a recording's card** selects the part whose MCPS `location` equals the card's `<a href>`
  (`Recording::link`, `RecapView::show_recording`). The synthetic fixture builds both from one link, so the test passes
  by construction; if the live href differs (a query, another segment), the card opens Part 1 instead of its own.

Suggested next step: a `meeting-steps` run (lab account a alone, nobody invited, as the recap's own capture was) that
records a meeting more times than a page holds, or a capture of a long-lived lab meeting chat's recap, to see a paged
answer and the next-page read; and from the same capture, compare a ready recording message's href with its MCPS
`location`. Map the next-page read as a `Read` and follow it in `teams_core::recap`; record the href's form in
`recap.md` and give the fixture its captured shape (matching on the item, `driveItemId`, if they differ).
