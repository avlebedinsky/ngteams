# The Chat dot in a 1:1 call does not come on the mock

In `large_call_window`'s step `qa-chat-calls` (`crates/ngteams/tests/support/linux/large_call_qa_chat.rs`,
`one_to_one`), b answers a ring from a to a 1:1 call and then posts to the a-b chat (`ChatServiceMock::post_as`). The
bubble shows in the call window, but the red dot on Chat does not come. Thus the step checks only the bubble now.
The dot comes in the group call and in the Meet now of the same harness. The view test
`teams-ui/tests/calls/chat_badge.rs` shows the dot in a 1:1 call with a fake core.

What was seen (task #666, review round 1, on 2026-11-20):

- Before the call, with the main window on the a-b-c chat (`Request::Open`), a post of b to the a-b chat leaves the
  a-b chat at 0 unread (`ngteamsctl unread`, that is, the tray's count from `Store::unread_count`).
- The app sends no `consumptionhorizon` for the a-b chat to the mock. It sends one only for the a-b-c chat, which the
  main window shows.
- The fixture's horizon of the a-b chat (`1760000300000;…`) is before the mock's clock (`CLOCK_START`
  1760001000000), so the post of b is after it.

Find why the store counts the post as read: a local read horizon of the a-b chat, the account's mark-all-read
horizon, or a message row that the count does not see. If the cause is in the app, a user does not get the dot in a
1:1 call, and that is a bug. Then assert the dot in `one_to_one` again, with no condition.
