// The personal build's fence can't be opened from a lab binary: `Fence::open_personal` and the `personal` module exist
// only with teams-fence's `personal` feature, which only `crates/teams` (the personal binary) enables.
use teams_fence::Fence;
use teams_fence::personal::PersonalPaths;

fn main() {
    let paths = PersonalPaths::from_env().unwrap();
    let _ = Fence::open_personal(&paths, "someone@example.com");
}
