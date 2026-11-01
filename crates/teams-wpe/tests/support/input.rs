//! History buttons preserve the native WPE press/release mapping.

use super::*;

#[test]
fn history_buttons_keep_their_wpe_numbers_for_presses_and_releases() {
    for (button, number) in [(Button::Back, 8), (Button::Forward, 9)] {
        assert_eq!(button.number(), number);
        assert_eq!(PointerKind::Down(button).event_type(), platform::WPE_EVENT_POINTER_DOWN);
        assert_eq!(PointerKind::Up(button).event_type(), platform::WPE_EVENT_POINTER_UP);
    }
}
