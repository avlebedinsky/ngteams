//! The Call health pane (More → Settings → Call health), as Teams shows it beside the stage: whether the call receives
//! data and when its measures were last read, then a card each for the network, audio, video and screen sharing with
//! their main measures and "View more … data", which opens the section's page: a card per measure with what it means.
//! A measure not known (no video, nothing shared) reads "--", as Teams writes it. The measures are read again every
//! [`HEALTH_REFRESH`] while the pane is open; they are the call's media's own (`Core::call_health`, libwebrtc's
//! statistics): reading them sends nothing and isn't a call action.

use std::time::Duration;

use gpui::{
    AnyElement, AsyncApp, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Task, WeakEntity, div, px, rgb,
};
use gpui_component::Sizable as _;
use gpui_component::menu::PopupMenuItem;
use gpui_component::tooltip::Tooltip;
use teams_core::calling::StreamHealth;
use teams_core::{CallHealth, CallId};

use super::{CallStage, PANE_WIDTH, Pane};
use crate::clock::{self, HourCycle};
use crate::icons::{Glyph, icon_button, selected_by};
use crate::theme::Palette;

/// How often the open pane reads the call's health again.
pub const HEALTH_REFRESH: Duration = Duration::from_secs(2);

/// What a measure not known reads, as Teams writes it.
const UNKNOWN: &str = "--";

/// The pane's state while it is open: the call it reads, what it read last and when, the section's page open, and its
/// reading.
pub(super) struct HealthPane {
    call: CallId,
    latest: Option<(CallHealth, chrono::DateTime<chrono::Local>)>,
    page: Option<Section>,
    _reading: Task<()>,
}

/// A section of the pane, in Teams' order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Section {
    Network,
    Audio,
    Video,
    Sharing,
}

impl Section {
    const ALL: [Self; 4] = [Self::Network, Self::Audio, Self::Video, Self::Sharing];

    fn title(self) -> &'static str {
        match self {
            Self::Network => "Network",
            Self::Audio => "Audio",
            Self::Video => "Video",
            Self::Sharing => "Screen Sharing",
        }
    }

    /// Its card's selector, its "View more" row's and its page's.
    fn selectors(self) -> [&'static str; 3] {
        match self {
            Self::Network => [
                "call-health-network",
                "call-health-more-network",
                "call-health-page-network",
            ],
            Self::Audio => ["call-health-audio", "call-health-more-audio", "call-health-page-audio"],
            Self::Video => ["call-health-video", "call-health-more-video", "call-health-page-video"],
            Self::Sharing => [
                "call-health-sharing",
                "call-health-more-sharing",
                "call-health-page-sharing",
            ],
        }
    }

    fn more(self) -> &'static str {
        match self {
            Self::Network => "View more network data",
            Self::Audio => "View more audio data",
            Self::Video => "View more video data",
            Self::Sharing => "View more screen sharing data",
        }
    }

    /// What the section's title explains, where Teams' has an info mark.
    fn about(self) -> Option<&'static str> {
        match self {
            Self::Network | Self::Audio => None,
            Self::Video => Some("The camera's video this call sends and receives"),
            Self::Sharing => Some("The screen this call shares, or the one shared with it"),
        }
    }
}

/// One measure: its name, its value, and what it means (its info mark's tip).
struct Metric {
    name: &'static str,
    value: SharedString,
    about: &'static str,
}

impl CallStage {
    /// Opens or closes the Call health pane, as More → Settings → Call health does; it opens on its overview.
    pub fn show_health(&mut self, open: bool, cx: &mut Context<Self>) {
        if open {
            self.pane = Pane::Health;
            self.effects = None;
            match self.health.as_mut().filter(|pane| pane.call == self.call) {
                Some(pane) => pane.page = None,
                None => {
                    self.health = Some(HealthPane {
                        call: self.call,
                        latest: None,
                        page: None,
                        _reading: read_health(self, cx),
                    });
                }
            }
        } else if self.pane == Pane::Health {
            self.pane = Pane::None;
            self.health = None;
        }
        cx.notify();
    }

    /// Opens the Call health pane showing `health` until a read brings another: the UI tour's, whose engine runs no
    /// call media.
    pub fn show_health_reading(&mut self, health: CallHealth, cx: &mut Context<Self>) {
        self.show_health(true, cx);
        if let Some(pane) = self.health.as_mut() {
            pane.latest = Some((health, chrono::Local::now()));
        }
    }

    /// Whether the Call health pane is open.
    #[must_use]
    pub fn health_shown(&self) -> bool {
        self.pane == Pane::Health
    }

    /// Opens the page of the pane's section numbered `section` (Network, Audio, Video, Screen Sharing from 0), as its
    /// View more does: the UI tour's.
    pub fn show_health_section(&mut self, section: usize, cx: &mut Context<Self>) {
        self.open_health_page(Section::ALL.get(section).copied(), cx);
    }

    fn open_health_page(&mut self, page: Option<Section>, cx: &mut Context<Self>) {
        if let Some(pane) = self.health.as_mut() {
            pane.page = page;
            cx.notify();
        }
    }

    /// The Call health pane while it is open: its overview, or a section's page.
    pub(super) fn health_pane(&mut self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.pane != Pane::Health {
            // another pane took its place: it reads no more
            self.health = None;
            return None;
        }
        let (page, latest) = self.health.as_ref().map(|open| (open.page, open.latest.clone()))?;
        let close = selected_by(
            "call-health-close",
            icon_button("call-health-close", Glyph::X, "Close")
                .small()
                .on_click(cx.listener(|this, _, _, cx| this.show_health(false, cx))),
        );
        let health = latest.as_ref().map(|(health, _)| health);
        let (back, title, body) = match page {
            Some(section) => {
                let back = selected_by(
                    "call-health-back",
                    icon_button("call-health-back", Glyph::CHEVRON_LEFT, "Back")
                        .small()
                        .on_click(cx.listener(|this, _, _, cx| this.open_health_page(None, cx))),
                )
                .into_any_element();
                let cards = details(section, health)
                    .into_iter()
                    .map(|metric| metric_card(metric, palette));
                let body = div()
                    .debug_selector(move || section.selectors()[2].to_owned())
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .children(cards)
                    .into_any_element();
                (Some(back), section.title(), body)
            }
            None => (None, "Call health", Self::overview(latest.as_ref(), palette, cx)),
        };
        Some(frame(back, title, close.into_any_element(), body, palette))
    }

    /// The overview: whether the call receives data, when it was read, and a card per section.
    fn overview(
        latest: Option<&(CallHealth, chrono::DateTime<chrono::Local>)>,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let receiving = latest.is_some_and(|(health, _)| receives(health));
        let intro = if receiving {
            "Your call is receiving data. Most metrics refresh every 2 seconds."
        } else {
            "Your call isn't receiving data yet. Most metrics refresh every 2 seconds."
        };
        let updated = latest.map(|(_, at)| {
            div()
                .debug_selector(|| "call-health-updated".to_owned())
                .child(SharedString::from(format!(
                    "Last updated at: {}",
                    at.format(seconds_format(clock::cycle(cx)))
                )))
        });
        let health = latest.map(|(health, _)| health);
        let cards: Vec<AnyElement> = Section::ALL
            .into_iter()
            .map(|section| Self::section_card(section, health, palette, cx))
            .collect();
        div()
            .flex()
            .flex_col()
            .gap_2p5()
            .child(
                div()
                    .debug_selector(|| "call-health-intro".to_owned())
                    .pt_3()
                    .pb_1()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .text_xs()
                    .text_color(rgb(palette.foreground))
                    .child(intro)
                    .children(updated),
            )
            .children(cards)
            .into_any_element()
    }

    /// A section's card: its title (with its info mark), its main measures as "Name: value", then "View more … data".
    fn section_card(
        section: Section,
        health: Option<&CallHealth>,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let [card, more, _] = section.selectors();
        let lines = summary(section, health).into_iter().map(|(name, value)| {
            div()
                .text_sm()
                .text_color(rgb(palette.foreground))
                .child(SharedString::from(format!("{name}: {value}")))
        });
        let content = div()
            .p_2p5()
            .flex()
            .flex_col()
            .gap_1()
            .bg(rgb(palette.hover))
            .child(title_with_info(card, section.title(), section.about(), palette).mb_1p5())
            .children(lines);
        let view_more = div()
            .id(more)
            .debug_selector(move || more.to_owned())
            .px_2p5()
            .py_3()
            .flex()
            .items_center()
            .justify_between()
            .bg(rgb(palette.hover))
            .cursor_pointer()
            .hover(|style| style.bg(rgb(palette.selected)))
            .text_sm()
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .child(section.more())
            .child(Glyph::CHEVRON_RIGHT.icon().size(px(16.0)))
            .on_click(cx.listener(move |this, _, _, cx| this.open_health_page(Some(section), cx)));
        div()
            .debug_selector(move || card.to_owned())
            .flex()
            .flex_col()
            .gap_0p5()
            .rounded_md()
            .overflow_hidden()
            .child(content)
            .child(view_more)
            .into_any_element()
    }
}

/// More → Settings' Call health: opens the pane.
pub(super) fn menu_item(stage: WeakEntity<CallStage>) -> PopupMenuItem {
    super::toolbar::menu_item("call-more-call-health", Some(Glyph::ACTIVITY), "Call health").on_click(
        move |_, _, cx| {
            let _ = stage.update(cx, |stage, cx| stage.show_health(true, cx));
        },
    )
}

/// Reads the call's health now and every [`HEALTH_REFRESH`] after, until the pane closes (its task dropped) or the
/// stage goes.
fn read_health(stage: &CallStage, cx: &mut Context<CallStage>) -> Task<()> {
    let core = std::sync::Arc::clone(&stage.core);
    let call = stage.call;
    cx.spawn(async move |this, cx: &mut AsyncApp| {
        loop {
            let health = core.call_health(call).await.ok().flatten();
            let updated = this.update(cx, |this, cx| {
                // a read that brings nothing (the media going) leaves the last one shown
                if let Some(pane) = this.health.as_mut().filter(|pane| pane.call == call)
                    && let Some(health) = health
                {
                    pane.latest = Some((health, chrono::Local::now()));
                    cx.notify();
                }
            });
            if updated.is_err() {
                return;
            }
            cx.background_executor().timer(HEALTH_REFRESH).await;
        }
    })
}

/// The pane's frame, as the call window's side panes are: its header (Back on a section's page, the title, Close) over
/// its scrolling body.
fn frame(
    back: Option<AnyElement>,
    title: &'static str,
    close: AnyElement,
    body: AnyElement,
    palette: Palette,
) -> AnyElement {
    div()
        .debug_selector(|| "call-health-pane".to_owned())
        .w(px(PANE_WIDTH))
        .flex_none()
        .h_full()
        .flex()
        .flex_col()
        .bg(rgb(palette.surface))
        .border_l_1()
        .border_color(rgb(palette.border))
        .child(
            div()
                .h(px(48.0))
                .flex_none()
                .px_4()
                .flex()
                .items_center()
                .gap_2()
                .border_b_1()
                .border_color(rgb(palette.border))
                .children(back)
                .child(
                    div()
                        .flex_1()
                        .text_lg()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(title),
                )
                .child(close),
        )
        .child(
            div()
                .id("call-health-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .px_3()
                .pb_4()
                .pt_1()
                .child(body),
        )
        .into_any_element()
}

/// A bold name with an info mark whose tip says what it means (none: no mark).
fn title_with_info(
    selector: &'static str,
    name: &'static str,
    about: Option<&'static str>,
    palette: Palette,
) -> gpui::Div {
    let info = about.map(|about| {
        div()
            .id(SharedString::from(format!("{selector}-info")))
            .text_color(rgb(palette.foreground))
            .child(Glyph::INFO.icon().size(px(14.0)))
            .tooltip(move |window, cx| Tooltip::new(about).build(window, cx))
    });
    div()
        .flex()
        .items_center()
        .gap_1p5()
        .text_sm()
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(rgb(palette.foreground))
        .child(name)
        .children(info)
}

/// A section page's card: the measure's name with its info mark, its value below in larger type.
fn metric_card(metric: Metric, palette: Palette) -> AnyElement {
    let Metric { name, value, about } = metric;
    div()
        .p_2p5()
        .flex()
        .flex_col()
        .gap_2()
        .rounded_md()
        .bg(rgb(palette.hover))
        .child(title_with_info(name, name, Some(about), palette))
        .child(div().text_color(rgb(palette.foreground)).child(value))
        .into_any_element()
}

/// Whether the call receives anything.
fn receives(health: &CallHealth) -> bool {
    [&health.audio, &health.video, &health.sharing]
        .into_iter()
        .any(|media| media.received.as_ref().is_some_and(|received| received.packets > 0))
}

/// A time of day with its seconds, on the app's clock.
fn seconds_format(cycle: HourCycle) -> &'static str {
    match cycle {
        HourCycle::Twelve => "%-I:%M:%S %p",
        HourCycle::TwentyFour => "%H:%M:%S",
    }
}

/// A section card's main measures, as Teams lists them.
fn summary(section: Section, health: Option<&CallHealth>) -> Vec<(&'static str, SharedString)> {
    let names: &[&'static str] = match section {
        Section::Network => &["Round trip time", "Received packet loss"],
        Section::Audio => &["Sent bitrate", "Received Jitter"],
        Section::Video => &[
            "Sent bitrate",
            "Received bitrate",
            "Sent frame rate",
            "Sent resolution",
            "Sent codec",
            "Video processing",
        ],
        Section::Sharing => &[
            "Sent bitrate",
            "Sent frame rate",
            "Sent resolution",
            "Sent codec",
            "Screen share send processing",
            "Received bitrate",
            "Received frame rate",
            "Received resolution",
            "Received codec",
            "Screen share received processing",
        ],
    };
    let all = details(section, health);
    names
        .iter()
        .map(|name| {
            let value = all
                .iter()
                .find(|metric| metric.name == *name)
                .map_or_else(|| SharedString::from(UNKNOWN), |metric| metric.value.clone());
            (*name, value)
        })
        .collect()
}

/// A section page's measures, as Teams lists them.
fn details(section: Section, health: Option<&CallHealth>) -> Vec<Metric> {
    let network = health.map(|health| &health.network);
    let rtt = network.and_then(|network| network.round_trip_ms);
    match section {
        Section::Network => vec![
            metric(
                "Teams send limit",
                mbps(network.and_then(|n| n.available_send_kbps)),
                SEND_LIMIT,
            ),
            metric(
                "Teams receive limit",
                mbps(network.and_then(|n| n.available_receive_kbps)),
                RECEIVE_LIMIT,
            ),
            metric("Round trip time", ms(rtt), ROUND_TRIP),
            metric(
                "Received packet loss",
                percent(network.and_then(|n| n.received_loss_percent)),
                LOSS,
            ),
        ],
        Section::Audio => {
            let media = health.map(|health| &health.audio);
            let (sent, received) = ways(media);
            vec![
                metric("Sent bitrate", kbps(sent.and_then(|s| s.bitrate_kbps)), SENT_BITRATE),
                metric("Sent packets", packets(sent), SENT_PACKETS),
                metric(
                    "Round trip time",
                    ms(sent.and_then(|s| s.round_trip_ms).or(rtt)),
                    ROUND_TRIP,
                ),
                metric("Sent codec", codec(sent), SENT_CODEC),
                metric("Received Jitter", ms(received.and_then(|r| r.jitter_ms)), JITTER),
                metric(
                    "Received packet loss",
                    percent(received.and_then(|r| r.loss_percent)),
                    LOSS,
                ),
                metric("Received packets", packets(received), RECEIVED_PACKETS),
                metric("Received codec", codec(received), RECEIVED_CODEC),
            ]
        }
        Section::Video => video(health.map(|health| &health.video), rtt),
        Section::Sharing => sharing(health.map(|health| &health.sharing), rtt),
    }
}

fn video(media: Option<&teams_core::calling::MediaHealth>, rtt: Option<f64>) -> Vec<Metric> {
    let (sent, received) = ways(media);
    vec![
        metric("Sent bitrate", kbps(sent.and_then(|s| s.bitrate_kbps)), SENT_BITRATE),
        metric(
            "Received bitrate",
            kbps(received.and_then(|r| r.bitrate_kbps)),
            RECEIVED_BITRATE,
        ),
        metric("Sent frame rate", fps(sent), SENT_FPS),
        metric("Sent resolution", resolution(sent), SENT_RESOLUTION),
        metric(
            "Round trip time",
            ms(sent.and(sent.and_then(|s| s.round_trip_ms).or(rtt))),
            ROUND_TRIP,
        ),
        metric("Sent packets", packets(sent), SENT_PACKETS),
        metric("Sent codec", codec(sent), SENT_CODEC),
        metric("Video processing", processing(sent), SEND_PROCESSING),
    ]
}

fn sharing(media: Option<&teams_core::calling::MediaHealth>, rtt: Option<f64>) -> Vec<Metric> {
    let (sent, received) = ways(media);
    vec![
        metric("Sent bitrate", kbps(sent.and_then(|s| s.bitrate_kbps)), SENT_BITRATE),
        metric("Sent frame rate", fps(sent), SENT_FPS),
        metric("Sent resolution", resolution(sent), SENT_RESOLUTION),
        metric(
            "Round trip time",
            ms(sent.and(sent.and_then(|s| s.round_trip_ms).or(rtt))),
            ROUND_TRIP,
        ),
        metric("Sent packets", packets(sent), SENT_PACKETS),
        metric("Sent codec", codec(sent), SENT_CODEC),
        metric("Screen share send processing", processing(sent), SEND_PROCESSING),
        metric(
            "Received bitrate",
            kbps(received.and_then(|r| r.bitrate_kbps)),
            RECEIVED_BITRATE,
        ),
        metric("Received frame rate", fps(received), RECEIVED_FPS),
        metric("Received resolution", resolution(received), RECEIVED_RESOLUTION),
        metric(
            "Received packet loss",
            percent(received.and_then(|r| r.loss_percent)),
            LOSS,
        ),
        metric("Received packets", packets(received), RECEIVED_PACKETS),
        metric("Received codec", codec(received), RECEIVED_CODEC),
        metric(
            "Screen share received processing",
            processing(received),
            RECEIVE_PROCESSING,
        ),
    ]
}

const SEND_LIMIT: &str = "How much the connection lets this call send now, as its bandwidth estimate has it";
const RECEIVE_LIMIT: &str = "How much the connection lets this call receive now, as its bandwidth estimate has it";
const ROUND_TRIP: &str = "How long data takes to reach the other end and come back";
const LOSS: &str = "The share of the packets sent to you that never arrived";
const JITTER: &str = "How unevenly the packets sent to you arrive";
const SENT_BITRATE: &str = "How much data this call sends a second";
const RECEIVED_BITRATE: &str = "How much data this call receives a second";
const SENT_PACKETS: &str = "Packets this call sent since it started";
const RECEIVED_PACKETS: &str = "Packets this call received since it started";
const SENT_CODEC: &str = "How what this call sends is compressed";
const RECEIVED_CODEC: &str = "How what this call receives is compressed";
const SENT_FPS: &str = "Frames sent a second";
const RECEIVED_FPS: &str = "Frames received a second";
const SENT_RESOLUTION: &str = "The size of the picture sent";
const RECEIVED_RESOLUTION: &str = "The size of the picture received";
const SEND_PROCESSING: &str = "Whether the picture sent is encoded in hardware or software";
const RECEIVE_PROCESSING: &str = "Whether the picture received is decoded in hardware or software";

fn metric(name: &'static str, value: SharedString, about: &'static str) -> Metric {
    Metric { name, value, about }
}

fn ways(media: Option<&teams_core::calling::MediaHealth>) -> (Option<&StreamHealth>, Option<&StreamHealth>) {
    (
        media.and_then(|media| media.sent.as_ref()),
        media.and_then(|media| media.received.as_ref()),
    )
}

fn known(value: Option<String>) -> SharedString {
    value.map_or_else(|| SharedString::from(UNKNOWN), SharedString::from)
}

fn ms(value: Option<f64>) -> SharedString {
    known(value.map(|ms| format!("{ms:.2} ms")))
}

fn percent(value: Option<f64>) -> SharedString {
    known(value.map(|percent| format!("{percent:.2}%")))
}

/// kbit/s with up to two decimals ("2.5 Kbps", "2.36 Kbps").
fn kbps(value: Option<f64>) -> SharedString {
    known(value.map(|kbps| {
        let text = format!("{kbps:.2}");
        let text = text.trim_end_matches('0').trim_end_matches('.');
        format!("{text} Kbps")
    }))
}

fn mbps(value: Option<f64>) -> SharedString {
    known(value.map(|kbps| format!("{:.2} Mbps", kbps / 1000.0)))
}

fn packets(stream: Option<&StreamHealth>) -> SharedString {
    known(stream.map(|stream| format!("{} packets", stream.packets)))
}

fn fps(stream: Option<&StreamHealth>) -> SharedString {
    known(
        stream
            .and_then(|stream| stream.frame_rate)
            .map(|fps| format!("{fps:.0} fps")),
    )
}

fn resolution(stream: Option<&StreamHealth>) -> SharedString {
    known(
        stream
            .and_then(|stream| stream.resolution)
            .map(|(width, height)| format!("{width}x{height}")),
    )
}

fn codec(stream: Option<&StreamHealth>) -> SharedString {
    known(stream.and_then(|stream| stream.codec.as_deref()).map(str::to_uppercase))
}

fn processing(stream: Option<&StreamHealth>) -> SharedString {
    known(
        stream
            .and_then(|stream| stream.hardware)
            .map(|hardware| if hardware { "Hardware" } else { "Software" }.to_owned()),
    )
}
