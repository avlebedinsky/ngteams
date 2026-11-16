# Server: one admin audit entry type for accounts and apps

`crates/ngteams-server/src/apps/repo.rs` has its own copy of the `admin_audit` row type (`Entry`), the `record!`
macro and `trace`, the same as `NewEntry`, `record!` and `trace` in `crates/ngteams-server/src/admin/repo.rs`. A
change to the audit row in one place can miss the other.

Make `admin::repo::NewEntry` (with `new`), `record!` and `trace` `pub(crate)`, call them from `apps/repo.rs` with
the app's id and name as the target, and remove the copies. Do it when no other task changes `admin/repo.rs`.
