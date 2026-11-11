---
worth: yes
where: crates/teams-proto/src/loki.rs, crates/teams-ui/src/profile_card/draw.rs
added: 2026-11-11
---
# Profile card: the out-of-office message

When someone's automatic replies are on, Teams' card shows their out-of-office message. The app's card says "Out of
office" (and until when, from their schedule) but not the message: Loki's `api/v1/oofstatus` was only captured with
automatic replies off (`{"outOfOfficeState":"Disabled",…}`), so the shape of an enabled answer and its message (HTML
or text, internal or external audience) is unknown. Capture it with a lab account's automatic replies on, then read it
(`LokiOutOfOffice`) and draw it from sanitized `RichText`.
