//! What GPUI's own `show_system_notification` does on the bus the app is given (`harness = false`: GPUI runs on the
//! process's main thread). M2 Task 14 asked this first, to choose the notification path.
//!
//! The test runs itself again inside the UI sandbox (its compositor, its private bus with the fake notification
//! server, nothing of the desktop's). The child posts two notifications through GPUI with the same tag and one
//! action each, and prints every response GPUI hands back. The parent reads what the fake server got and activates the
//! first one's body.
//!
//! What it shows: GPUI's notifications reach `org.freedesktop.Notifications` on the sandbox's bus, with their actions
//! (`default` first, then GPUI's own action keys), and the body's activation comes back with its tag. But the tag
//! replaces nothing (each `Notify` has `replaces_id` 0) and nothing can close one (`dismiss_system_notification`
//! does nothing on Linux), so more than three messages of one chat can't collapse into one; and each notification
//! opens a connection of its own with `zbus::Connection::session()` (the environment's address, or
//! `$XDG_RUNTIME_DIR/bus` without one) on a thread that waits until the server closes it. So the app sends its own
//! over `zbus`, on the address it is given (`teams_ui::notify`); this test keeps GPUI's behaviour on record.
//!
//! Without weston 15, niri or `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::io::{self, Write as _};
use std::process::ExitCode;
use std::time::Duration;

use gpui::{App, AsyncApp, SystemNotification, SystemNotificationAction, WindowingModes};
use ui_sandbox::{Running, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until};

/// Which mode the child runs in.
const CHILD_ENV: &str = "TEAMS_UI_GPUI_NOTIFICATION_TEST";
/// From the child's start to its notifications on the bus.
const STARTUP: Duration = Duration::from_secs(90);
/// Anything else the parent waits for.
const WAIT: Duration = Duration::from_secs(20);
/// How long the child waits for a response before it quits.
const CHILD_WAIT: Duration = Duration::from_secs(30);

fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV).as_deref() {
        Ok("notify") => child(),
        Ok(other) => Err(format!("unknown mode {other}").into()),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("gpui notification test: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Prints `line` to stdout at once.
fn say(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

fn parent() -> SandboxResult {
    let Some(sandbox) = sandbox_or_skip("gpui notification", &SandboxOptions::default())? else {
        return Ok(());
    };
    let child = sandbox.spawn(&std::env::current_exe()?, &[], &[(CHILD_ENV, "notify")])?;
    let running = Running::new(child)?;
    let bus = sandbox.bus();
    let notifications = wait_until(STARTUP, || {
        let got = bus.notifications();
        (got.len() >= 2).then_some(got)
    })
    .ok_or_else(|| format!("GPUI's notifications didn't reach the bus: {:?}", bus.notifications()))?;
    for notification in &notifications {
        // the body's action first, then GPUI's key for ours with its label
        assert_eq!(notification.actions, ["default", "default", "gpui-action-0", "Open"]);
        assert_eq!(
            (notification.summary.as_str(), notification.body.as_str()),
            ("Ann", "hello")
        );
        // the same tag replaced nothing
        assert_eq!(notification.replaces_id, 0, "{notification:?}");
    }
    assert_ne!(notifications[0].id, notifications[1].id);
    // each one from a connection of its own
    assert_ne!(notifications[0].sender, notifications[1].sender);
    bus.invoke_action(notifications[0].id, "default")?;
    let response = running
        .line_where(WAIT, |line| line.starts_with("response "))
        .map_err(|error| format!("no response line: {error:?}"))?;
    assert_eq!(response, "response chat-1 body");
    let (status, stderr) = running.finish(WAIT)?;
    if !status.success() {
        return Err(format!("the child exited {status}: {stderr}").into());
    }
    say("gpui notification: ok");
    Ok(())
}

fn child() -> SandboxResult {
    ui_sandbox::window_allowed()?;
    gpui_platform::linux(WindowingModes::WAYLAND).run(|cx: &mut App| {
        cx.on_system_notification_response(|response, cx| {
            let action = response
                .action_id
                .map_or_else(|| "body".to_owned(), |id| id.to_string());
            say(&format!("response {} {action}", response.tag));
            cx.quit();
        });
        for _ in 0..2 {
            cx.show_system_notification(SystemNotification {
                tag: "chat-1".into(),
                title: "Ann".into(),
                body: "hello".into(),
                actions: vec![SystemNotificationAction {
                    id: "open".into(),
                    label: "Open".into(),
                }],
            });
        }
        cx.spawn(async move |cx: &mut AsyncApp| {
            cx.background_executor().timer(CHILD_WAIT).await;
            say("no response");
            cx.update(|cx| cx.quit());
        })
        .detach();
    });
    Ok(())
}
