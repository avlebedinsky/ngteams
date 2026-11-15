# An organizer's occurrence change isn't serialized with a reshape of its series

The own server's calendar (`crates/ngteams-server/src/calendar/repo.rs`, `own_row`) holds the series' row and checks
its version only for an attendee's answer or removal (`keep_own_row`). The organizer's change or cancellation of one
occurrence (`put_own_row`) does neither. When the same organizer sends two requests at once from two clients, a
change of one occurrence and a change of the series' rule or times, the occurrence's row can be written after the
reshape forgot the rows under the series. The reshaped series then carries an exception of the old rule, with the
invitations the occurrence had. This is a rare minor issue: it needs one organizer's two concurrent edits, and a
later change of the series or of that occurrence repairs it.

Hold the series' row in `own_row` for the organizer's path too and compare what is there with what the organizer
read: the series' version when the occurrence had no row, the row's own when it had one. The callers of
`Found::own_row` (`write::occurrence_changed`, `write::end`, `group::cancel`) give its row a new version, so the
version read has to be passed beside it. On a mismatch answer 409 `Conflict`, or read and apply again as
`write::invited_rows` does for an attendee. Both callers need it: `write::occurrence_changed` and `write::end` for
`me/calendars`, and `group::update` and `group::cancel` for a channel's calendar.

The interleaving cannot be forced over HTTP. Cover it with a repo unit test in the style of
`a_row_isnt_kept_from_a_series_read_before_it_was_cancelled`, and run the Postgres variant, where the row hold does
its work.
