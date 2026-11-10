//! Live-process `WinRT` notifications, with explicit replacement, withdrawal and global account-safe IDs.
use super::{Apartment, registration};
use crate::notify::{
    ANSWER_ACTION, DECLINE_ACTION, DEFAULT_ACTION, Notice, NoticeKind, NotificationServer, escape_markup,
};
use futures_util::future::BoxFuture;
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};
use tokio::sync::oneshot;
use windows::{
    Data::Xml::Dom::XmlDocument,
    Foundation::TypedEventHandler,
    UI::Notifications::{
        ToastActivatedEventArgs, ToastDismissalReason, ToastDismissedEventArgs, ToastNotification,
        ToastNotificationManager, ToastNotifier,
    },
    core::{HSTRING, IInspectable, Interface as _},
};

type Activation = (u32, String);
type Events = async_channel::Receiver<Activation>;
enum Command {
    Show(Notice, u32, oneshot::Sender<Result<u32, String>>),
    Close(u32),
    Forget(u32, Arc<AtomicBool>),
    Stop,
}
/// Commands are serialized on an MTA thread. The final owner explicitly stops it, regardless of callback senders.
pub(crate) struct Notifications {
    commands: mpsc::Sender<Command>,
    thread: std::sync::Mutex<Option<std::thread::JoinHandle<()>>>,
}
impl Notifications {
    pub(super) fn stop(&self) {
        let _ = self.commands.send(Command::Stop);
        if let Some(thread) = self
            .thread
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            let _ = thread.join();
        }
    }
}
impl Drop for Notifications {
    fn drop(&mut self) {
        self.stop();
    }
}
impl Notifications {
    pub(crate) async fn start(app_id: String, name: String) -> Result<(Self, Events), String> {
        let (commands, queue) = mpsc::channel();
        let (events, activations) = async_channel::unbounded();
        let (ready, started) = oneshot::channel();
        // Cancellation before the frontend is returned must leave no detached native worker.
        let (activate, opened) = mpsc::channel();
        let callbacks = commands.clone();
        let thread = std::thread::Builder::new()
            .name("ngteams-toasts".into())
            .spawn(move || {
                let result = Apartment::new().map_err(|e| e.to_string()).and_then(|apartment| {
                    let registered = registration::register(&app_id, &name).map_err(|e| e.to_string())?;
                    let center = Center::new(app_id, callbacks, events).map_err(|e| e.to_string())?;
                    Ok((apartment, registered, center))
                });
                match result {
                    Ok((_apartment, _registered, mut center)) => {
                        if ready.send(Ok(())).is_ok() && opened.recv().is_ok() {
                            center.run(&queue);
                        }
                    }
                    Err(error) => {
                        let _ = ready.send(Err(error));
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        started.await.map_err(|_| "toast thread stopped".to_owned())??;
        let frontend = Self {
            commands,
            thread: std::sync::Mutex::new(Some(thread)),
        };
        let _ = activate.send(());
        Ok((frontend, activations))
    }
}
impl NotificationServer for Notifications {
    fn show(&self, notice: &Notice, replaces: u32) -> BoxFuture<'static, Result<u32, String>> {
        let (reply, result) = oneshot::channel();
        let sent = self.commands.send(Command::Show(notice.clone(), replaces, reply));
        Box::pin(async move {
            sent.map_err(|_| "toast thread stopped".to_owned())?;
            result.await.map_err(|_| "toast thread stopped".to_owned())?
        })
    }
    fn close(&self, id: u32) -> BoxFuture<'static, ()> {
        let _ = self.commands.send(Command::Close(id));
        Box::pin(async {})
    }
}
struct Shown {
    toast: ToastNotification,
    live: Arc<AtomicBool>,
}
struct Center {
    app_id: HSTRING,
    // A private group per run; neither tenant IDs nor message text enter toast launch arguments/tags.
    group: HSTRING,
    notifier: ToastNotifier,
    commands: mpsc::Sender<Command>,
    events: async_channel::Sender<Activation>,
    shown: HashMap<u32, Shown>,
    next: u32,
}
impl Center {
    fn new(
        app_id: String,
        commands: mpsc::Sender<Command>,
        events: async_channel::Sender<Activation>,
    ) -> windows::core::Result<Self> {
        let app_id = HSTRING::from(app_id);
        Ok(Self {
            notifier: ToastNotificationManager::CreateToastNotifierWithId(&app_id)?,
            app_id,
            group: HSTRING::from(format!("run-{}", std::process::id())),
            commands,
            events,
            shown: HashMap::new(),
            next: 0,
        })
    }
    fn run(&mut self, queue: &mpsc::Receiver<Command>) {
        while let Ok(command) = queue.recv() {
            match command {
                Command::Show(notice, replaces, reply) => {
                    let result = self
                        .show(&notice, replaces)
                        .map_err(|e| format!("Windows toast failed ({:#x})", e.code().0));
                    let _ = reply.send(result);
                }
                Command::Close(id) => self.close(id),
                Command::Forget(id, live) => {
                    if self.shown.get(&id).is_some_and(|shown| Arc::ptr_eq(&shown.live, &live)) {
                        self.shown.remove(&id);
                    }
                }
                Command::Stop => break,
            }
        }
        let ids: Vec<u32> = self.shown.keys().copied().collect();
        for id in ids {
            self.close(id);
        }
    }
    fn show(&mut self, notice: &Notice, replaces: u32) -> windows::core::Result<u32> {
        let id = if replaces != 0 && self.shown.contains_key(&replaces) {
            replaces
        } else {
            self.next = self
                .next
                .checked_add(1)
                .ok_or_else(|| windows::core::Error::from_hresult(windows::Win32::Foundation::E_UNEXPECTED))?;
            self.next
        };
        let document = XmlDocument::new()?;
        document.LoadXml(&HSTRING::from(xml(notice)))?;
        let toast = ToastNotification::CreateToastNotification(&document)?;
        toast.SetGroup(&self.group)?;
        toast.SetTag(&HSTRING::from(id.to_string()))?;
        toast.SetExpiresOnReboot(true)?;
        let live = Arc::new(AtomicBool::new(true));
        self.handlers(&toast, id, &live)?;
        self.notifier.Show(&toast)?;
        if let Some(old) = self.shown.insert(id, Shown { toast, live }) {
            old.live.store(false, Ordering::Release);
        }
        Ok(id)
    }
    fn handlers(&self, toast: &ToastNotification, id: u32, live: &Arc<AtomicBool>) -> windows::core::Result<()> {
        let (active, events) = (Arc::clone(live), self.events.clone());
        toast.Activated(&TypedEventHandler::<ToastNotification, IInspectable>::new(
            move |_, args| {
                let action = args
                    .as_ref()
                    .and_then(|arg| arg.cast::<ToastActivatedEventArgs>().ok())
                    .and_then(|arg| arg.Arguments().ok())
                    .map(|arg| arg.to_string_lossy());
                if active.load(Ordering::Acquire)
                    && let Some(action) = action.filter(|a| valid_action(a))
                {
                    let _ = events.try_send((id, action));
                }
                Ok(())
            },
        ))?;
        let (active, commands) = (Arc::clone(live), self.commands.clone());
        toast.Dismissed(&TypedEventHandler::<ToastNotification, ToastDismissedEventArgs>::new(
            move |_, args| {
                if args.as_ref().and_then(|a| a.Reason().ok()) != Some(ToastDismissalReason::TimedOut) {
                    active.store(false, Ordering::Release);
                    let _ = commands.send(Command::Forget(id, Arc::clone(&active)));
                }
                Ok(())
            },
        ))?;
        Ok(())
    }
    fn close(&mut self, id: u32) {
        if let Some(shown) = self.shown.remove(&id) {
            shown.live.store(false, Ordering::Release);
            let _ = self.notifier.Hide(&shown.toast);
            if let Ok(history) = ToastNotificationManager::History() {
                let _ = history.RemoveGroupedTagWithId(&HSTRING::from(id.to_string()), &self.group, &self.app_id);
            }
        }
    }
}
fn valid_action(action: &str) -> bool {
    matches!(action, DEFAULT_ACTION | ANSWER_ACTION | DECLINE_ACTION)
}

/// Only fixed actions are launch arguments. Display text is XML-escaped and remains plain text.
fn xml(notice: &Notice) -> String {
    let call = notice.kind == NoticeKind::Call;
    let scenario = if call {
        " scenario=\"incomingCall\" duration=\"long\""
    } else {
        ""
    };
    let actions = if call {
        r#"<actions><action content="Answer" arguments="answer" activationType="foreground"/><action content="Decline" arguments="decline" activationType="foreground"/></actions>"#
    } else {
        ""
    };
    // Message sounds follow Windows notification preferences; the app's ringer owns call audio.
    let audio = if call { r#"<audio silent="true"/>"# } else { "" };
    format!(
        r#"<toast launch="default"{scenario}><visual><binding template="ToastGeneric"><text>{}</text><text>{}</text></binding></visual>{actions}{audio}</toast>"#,
        escape_markup(&notice.summary),
        escape_markup(&notice.body)
    )
}
