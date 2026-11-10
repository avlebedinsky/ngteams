//! Actual native shell contract; no Teams services or account.
#[path = "../../src/desktop/windows/apartment.rs"]
mod apartment;
use apartment::Apartment;
#[path = "native_toast_driver.rs"]
mod driver;
#[path = "../../src/desktop/windows/registration.rs"]
mod registration;
#[path = "../../src/desktop/windows/toasts.rs"]
mod toasts;
#[path = "../../src/desktop/windows/tray.rs"]
mod tray;
use std::time::Duration;
use teams_ui::notify::{Notice, NoticeKind, NotificationServer as _};
use teams_ui::tray::{TrayEvent, TrayIcon as _};
use windows::{
    UI::Notifications::{ToastNotificationHistory, ToastNotificationManager},
    core::HSTRING,
};

pub fn main() -> std::process::ExitCode {
    if std::env::var_os("NGTEAMS_NATIVE_DESKTOP_REQUIRE").is_none() {
        println!("native desktop requires the authorized win11 console helper");
        return std::process::ExitCode::SUCCESS;
    }
    teams_platform::windows_sandbox::window_allowed_here().expect("authorized console");
    let _apartment = Apartment::new().expect("fixture apartment");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(75), contract())
            .await
            .expect("native desktop deadline");
    });
    std::process::ExitCode::SUCCESS
}
async fn contract() {
    let app_id = format!("com.nickalie.ngteams.synthetic.{}", std::process::id());
    teams_platform::windows_desktop::set_app_id(&app_id).unwrap();
    let (server, activations) = toasts::Notifications::start(app_id.clone(), "NGTEAMS SYNTHETIC DESKTOP".into())
        .await
        .unwrap();
    match ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(&app_id))
        .and_then(|notifier| notifier.Setting())
    {
        Ok(setting) => println!("synthetic system notification setting={}", setting.0),
        Err(error) => println!(
            "synthetic notification setting query unavailable ({:#x})",
            error.code().0
        ),
    }
    let (tray, events) = tray::NativeTray::start("NGTEAMS SYNTHETIC DESKTOP".into())
        .await
        .unwrap();
    assert!(matches!(events.recv().await.unwrap(), tray::Event::Hosted(true)));
    let notice = Notice {
        conversation: "synthetic-chat".into(),
        kind: NoticeKind::Message,
        summary: "NGTEAMS SYNTHETIC DESKTOP".into(),
        body: "Synthetic message <one> & two".into(),
    };
    history_contract(&server, &app_id, &notice).await;
    tray_contract(&tray, &events).await;
    activation_contract(&server, &activations, &notice).await;
    let history = ToastNotificationManager::History().unwrap();
    drop(server);
    wait_history(&history, &app_id, 0).await;
    drop(tray);
    while events.recv().await.is_ok() {}
    println!("native desktop contract passed");
}
async fn history_contract(server: &toasts::Notifications, app_id: &str, notice: &Notice) {
    let first = server.show(notice, 0).await.unwrap();
    let second = server.show(notice, 0).await.unwrap();
    assert_ne!(first, second, "all accounts share unique IDs");
    let updated = Notice {
        kind: NoticeKind::Collapsed,
        body: "4 new synthetic messages".into(),
        ..notice.clone()
    };
    assert_eq!(server.show(&updated, first).await.unwrap(), first);
    let history = ToastNotificationManager::History().unwrap();
    wait_history(&history, app_id, 2).await;
    let shown = history.GetHistoryWithId(&HSTRING::from(app_id)).unwrap();
    let contents: Vec<String> = (0..shown.Size().unwrap())
        .map(|i| {
            shown
                .GetAt(i)
                .unwrap()
                .Content()
                .unwrap()
                .GetXml()
                .unwrap()
                .to_string_lossy()
        })
        .collect();
    assert!(contents.iter().any(|xml| xml.contains("&lt;one&gt; &amp; two")));
    assert!(
        contents.iter().all(|xml| !xml.contains("silent=\"true\"")),
        "message sound follows the OS preference"
    );
    assert!(
        contents.iter().all(|xml| !xml.contains("synthetic-chat")),
        "routing IDs stay out of Windows launch arguments"
    );
    server.close(second).await;
    wait_history(&history, app_id, 1).await;
    server.close(first).await;
    wait_history(&history, app_id, 0).await;
}
async fn tray_contract(tray: &tray::NativeTray, events: &async_channel::Receiver<tray::Event>) {
    let hwnd = tray.test_window().await;
    assert_ne!(hwnd, 0);
    tray.set_window(Some(hwnd));
    tray.set_unread(7).await;
    assert!(tray.test_overlay().await, "real taskbar accepts the unread overlay");
    tray_activation_contract(tray, events).await;
    overlay_lifecycle_contract(tray).await;
}
async fn overlay_lifecycle_contract(tray: &tray::NativeTray) {
    tray.test_explorer_restart();
    tray.set_unread(9).await;
    assert!(tray.test_overlay().await);
    tray.set_unread(0).await;
    assert!(tray.test_overlay().await, "real taskbar clears the unread overlay");
    tray.set_window(None);
    let reopened = tray.test_window().await;
    tray.set_window(Some(reopened));
    tray.set_unread(12).await;
    assert!(tray.test_overlay().await, "overlay follows a recreated window");
}
async fn activation_contract(
    server: &toasts::Notifications,
    activations: &async_channel::Receiver<(u32, String)>,
    notice: &Notice,
) {
    for (title, action, kind) in [
        ("NGTEAMS SYNTHETIC ANSWER", "answer", NoticeKind::Call),
        ("NGTEAMS SYNTHETIC DECLINE", "decline", NoticeKind::Call),
        (
            "NGTEAMS SYNTHETIC OPEN",
            teams_ui::notify::DEFAULT_ACTION,
            NoticeKind::Message,
        ),
    ] {
        let call = Notice {
            kind,
            summary: title.into(),
            body: "Synthetic caller".into(),
            ..notice.clone()
        };
        let id = server.show(&call, 0).await.unwrap();
        println!("synthetic toast ready: {title}");
        driver::click(title, action).await;
        let actual = activations.recv().await.unwrap();
        assert_eq!(actual, (id, action.to_owned()), "real toast action carries its own ID");
        server.close(id).await;
    }
    let final_notice = Notice {
        summary: "NGTEAMS SYNTHETIC CLEANUP".into(),
        ..notice.clone()
    };
    server.show(&final_notice, 0).await.unwrap();
}
async fn wait_history(history: &ToastNotificationHistory, app_id: &str, count: u32) {
    for _ in 0..100 {
        if history
            .GetHistoryWithId(&HSTRING::from(app_id))
            .unwrap()
            .Size()
            .unwrap()
            == count
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    panic!("toast history did not reach {count}");
}

async fn tray_activation_contract(tray: &tray::NativeTray, events: &async_channel::Receiver<tray::Event>) {
    for event in [
        windows_sys::Win32::UI::Shell::NIN_SELECT,
        windows_sys::Win32::UI::Shell::NIN_SELECT | windows_sys::Win32::UI::Shell::NINF_KEY,
    ] {
        tray.test_callback(event);
        assert!(matches!(
            events.recv().await.unwrap(),
            tray::Event::Asked(TrayEvent::Open)
        ));
    }
}
