# Server: app package icons are written before the app row commits

`apps::add_package` and `apps::replace_package` (`crates/ngteams-server/src/apps/mod.rs`) write
`apps/<id>/color.png` and `outline.png` to the file store, and then change the database. When that change is refused
(`BotExists`, `AppExists` on a race of two first uploads, or a database failure), the icons stay in the store with
no app row, and nothing removes them. In a race of two first uploads, the registered app can show the icons of the
refused package.

Write the icons under a key for each upload (for example `apps/<id>/<upload id>/color.png`), set that key on the
app row in the same transaction, and delete the old or orphan keys after the commit. Add an e2e test that uploads a
package whose bot id is taken and checks that the store has no icons for it.
