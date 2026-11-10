//! Freedesktop notification transport, on the explicit Linux session bus.
use super::*;
use crate::app_icon;
use futures_util::{StreamExt as _, stream::BoxStream};
use zbus::{Connection, proxy::CacheProperties, zvariant::Value};

/// `org.freedesktop.Notifications` on the bus the app is given (see the module docs).
pub struct BusNotifications {
    proxy: zbus::Proxy<'static>,
    app_name: String,
    /// The app's desktop entry, without `.desktop`: the desktop finds the app's name and icon by it.
    desktop_entry: String,
    /// The server reads markup in a body: the text is escaped.
    markup: bool,
}

impl BusNotifications {
    /// The server on `connection`, notifying as `app_name` with the app's icon ([`app_icon::NAME`]) and the desktop
    /// entry `app_id` names, and its activations: each notification's id with the action invoked on it (its body's is
    /// [`DEFAULT_ACTION`]).
    ///
    /// # Errors
    /// The server isn't on the bus, or the subscription to its signals is refused.
    pub async fn start(
        connection: &Connection,
        app_name: &str,
        app_id: &str,
    ) -> zbus::Result<(Self, BoxStream<'static, (u32, String)>)> {
        let proxy: zbus::Proxy<'static> = zbus::proxy::Builder::new(connection)
            .destination(SERVER_NAME)?
            .path(SERVER_PATH)?
            .interface(SERVER_NAME)?
            .cache_properties(CacheProperties::No)
            .build()
            .await?;
        let capabilities: Vec<String> = proxy.call("GetCapabilities", &()).await?;
        let invoked = proxy.receive_signal("ActionInvoked").await?;
        let activations = invoked
            .filter_map(|signal| async move { signal.body().deserialize::<(u32, String)>().ok() })
            .boxed();
        let server = Self {
            proxy,
            app_name: app_name.to_owned(),
            desktop_entry: app_id.to_owned(),
            markup: capabilities.iter().any(|capability| capability == "body-markup"),
        };
        Ok((server, activations))
    }
}

impl NotificationServer for BusNotifications {
    fn show(&self, notice: &Notice, replaces: u32) -> BoxFuture<'static, Result<u32, String>> {
        let proxy = self.proxy.clone();
        let app_name = self.app_name.clone();
        let desktop_entry = self.desktop_entry.clone();
        let summary = notice.summary.clone();
        let body = if self.markup {
            escape_markup(&notice.body)
        } else {
            notice.body.clone()
        };
        let call_notice = notice.kind == NoticeKind::Call;
        Box::pin(async move {
            // an incoming call's: Answer and Decline, critical, shown until it is closed (its ring ended)
            let (actions, hints, expires) = if call_notice {
                let actions = vec![
                    DEFAULT_ACTION,
                    "Open",
                    ANSWER_ACTION,
                    "Answer",
                    DECLINE_ACTION,
                    "Decline",
                ];
                let hints: HashMap<&str, Value<'_>> = HashMap::from([
                    ("urgency", Value::U8(2)),
                    ("desktop-entry", Value::from(desktop_entry.as_str())),
                ]);
                (actions, hints, 0_i32)
            } else {
                let hints: HashMap<&str, Value<'_>> = HashMap::from([
                    ("category", Value::from("im.received")),
                    ("urgency", Value::U8(1)),
                    ("desktop-entry", Value::from(desktop_entry.as_str())),
                ]);
                (vec![DEFAULT_ACTION, "Open"], hints, -1_i32)
            };
            let call = (
                app_name.as_str(),
                replaces,
                app_icon::NAME,
                summary.as_str(),
                body.as_str(),
                actions,
                hints,
                expires,
            );
            proxy
                .call::<_, _, u32>("Notify", &call)
                .await
                .map_err(|error| error.to_string())
        })
    }

    fn close(&self, id: u32) -> BoxFuture<'static, ()> {
        let proxy = self.proxy.clone();
        Box::pin(async move {
            // one the server already closed is no matter
            let _ = proxy.call::<_, _, ()>("CloseNotification", &(id,)).await;
        })
    }
}
