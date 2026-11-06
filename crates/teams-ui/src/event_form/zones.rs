//! The scheduling form's time zones, as Teams' form shows them behind its globe: the start's date, time and time zone
//! on one line, the end's date, time and time zone under it. The event's slot stays in the desktop's time (the day
//! beside the form and the Scheduler draw it there); the zones only say in which zone its start and end are read and
//! picked. The middle tier's event has one zone, the start's (its Exchange name read by the engine,
//! `EventDraft::zone`), and reads both its times as that zone's clock whatever offset they carry (seen live
//! 2026-11-06: an end written with another zone's offset was taken as the start zone's time), so both are written in
//! the start's zone: an end picked in another zone is still the same moment.
//!
//! The zones listed are the IANA zones of the world's regions, each as "(UTC+03:00) Istanbul" at the event's start,
//! by offset then name.

use chrono::{DateTime, Duration, Local, NaiveDateTime, Offset as _, TimeZone as _, Utc};
use chrono_tz::OffsetComponents as _;
pub use chrono_tz::Tz;
use gpui::{App, Context, InteractiveElement as _, IntoElement, ParentElement as _, Styled as _, div, px};
use gpui_component::Sizable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use teams_core::ZonedTime;

use super::{EventForm, Slot, model, slot_item};
use crate::clock;
use crate::icons::selected_by;

/// The regions whose zones the pickers list (with UTC itself).
const REGIONS: [&str; 9] = [
    "Africa/",
    "America/",
    "Antarctica/",
    "Asia/",
    "Atlantic/",
    "Australia/",
    "Europe/",
    "Indian/",
    "Pacific/",
];

/// The start's and the end's time zones, while the form shows them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Zones {
    pub start: Tz,
    pub end: Tz,
}

impl Zones {
    /// Both the desktop's zone (UTC when it can't be told).
    #[must_use]
    pub fn desktop() -> Self {
        let zone = desktop_zone().unwrap_or(Tz::UTC);
        Self { start: zone, end: zone }
    }
}

/// The desktop's time zone: `TZ`, else where `/etc/localtime` links to.
#[must_use]
pub fn desktop_zone() -> Option<Tz> {
    if let Ok(zone) = std::env::var("TZ") {
        let zone = zone.trim_start_matches(':').trim();
        if let Ok(zone) = zone.parse() {
            return Some(zone);
        }
    }
    let target = std::fs::read_link("/etc/localtime").ok()?;
    let target = target.to_string_lossy();
    target.split_once("zoneinfo/")?.1.parse().ok()
}

/// The desktop's `local` time as `zone` shows it at the same moment.
#[must_use]
pub fn wall_in(local: NaiveDateTime, zone: Tz) -> NaiveDateTime {
    instant(local).with_timezone(&zone).naive_local()
}

/// The time `zone` shows as `wall`, in the desktop's time (the earlier of two when its clock goes back, the moment
/// after a gap when it skips).
#[must_use]
pub fn local_of(wall: NaiveDateTime, zone: Tz) -> NaiveDateTime {
    zone.from_local_datetime(&wall)
        .earliest()
        .or_else(|| zone.from_local_datetime(&(wall + Duration::hours(1))).earliest())
        .map_or(wall, |time| time.with_timezone(&Local).naive_local())
}

/// The desktop's `local` time as an instant written with `zone`'s offset then (the desktop's own without one).
#[must_use]
pub fn zoned_in(local: NaiveDateTime, zone: Option<Tz>) -> ZonedTime {
    let Some(zone) = zone else {
        return model::zoned(local);
    };
    let at = instant(local).with_timezone(&zone);
    ZonedTime {
        ms: at.timestamp_millis(),
        offset_minutes: at.offset().fix().local_minus_utc() / 60,
    }
}

/// The moment the desktop's `local` time names.
fn instant(local: NaiveDateTime) -> DateTime<Utc> {
    Local
        .from_local_datetime(&local)
        .earliest()
        .or_else(|| Local.from_local_datetime(&(local + Duration::hours(1))).earliest())
        .map_or_else(|| local.and_utc(), |time| time.with_timezone(&Utc))
}

/// A zone as the pickers name it at `at` (UTC): "(UTC+03:00) Istanbul", "(UTC) Coordinated Universal Time".
#[must_use]
pub fn label(zone: Tz, at: DateTime<Utc>) -> String {
    let name = zone.name();
    if zone == Tz::UTC {
        return "(UTC) Coordinated Universal Time".to_owned();
    }
    let city = name.rsplit('/').next().unwrap_or(name).replace('_', " ");
    format!("({}) {city}", offset_text(zone, at))
}

/// `zone`'s offset at `at`: "UTC+03:00", "UTC-04:30".
fn offset_text(zone: Tz, at: DateTime<Utc>) -> String {
    let offset = at.with_timezone(&zone).offset().base_utc_offset() + at.with_timezone(&zone).offset().dst_offset();
    let minutes = offset.num_minutes();
    let sign = if minutes < 0 { '-' } else { '+' };
    format!("UTC{sign}{:02}:{:02}", minutes.abs() / 60, minutes.abs() % 60)
}

/// The zones the pickers list at `at`, by offset then name, each name once.
#[must_use]
pub fn choices(at: DateTime<Utc>) -> Vec<(Tz, String)> {
    let mut zones: Vec<(i32, Tz, String)> = chrono_tz::TZ_VARIANTS
        .iter()
        .filter(|zone| *zone == &Tz::UTC || REGIONS.iter().any(|region| zone.name().starts_with(region)))
        .map(|zone| {
            let offset = at.with_timezone(zone).offset().fix().local_minus_utc();
            (offset, *zone, label(*zone, at))
        })
        .collect();
    zones.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.2.cmp(&b.2)));
    zones.dedup_by(|a, b| a.2 == b.2);
    zones.into_iter().map(|(_, zone, label)| (zone, label)).collect()
}

impl EventForm {
    /// The time zones shown, while they are.
    #[must_use]
    pub fn zones(&self) -> Option<Zones> {
        self.zones
    }

    /// Shows the start's and end's time zones (both the desktop's), or hides them: the event's times are the desktop's
    /// again.
    pub fn show_zones(&mut self, shown: bool, cx: &mut Context<Self>) {
        self.zones = shown.then(Zones::desktop);
        cx.notify();
    }

    /// The start's (`end` false) or the end's time zone changed to `zone`.
    pub fn set_zone(&mut self, end: bool, zone: Tz, cx: &mut Context<Self>) {
        let mut zones = self.zones.unwrap_or_else(Zones::desktop);
        if end {
            zones.end = zone;
        } else {
            zones.start = zone;
        }
        self.zones = Some(zones);
        cx.notify();
    }

    /// The start line's zone picker, or the end line's.
    pub(super) fn zone_menu(&self, end: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let zones = self.zones.unwrap_or_else(Zones::desktop);
        let chosen = if end { zones.end } else { zones.start };
        let at = instant(self.slot.start);
        let id = if end { "event-end-zone" } else { "event-start-zone" };
        // a zone and its alias are listed once: the chosen one is checked by its name
        let chosen_name = label(chosen, at);
        let form = cx.entity().downgrade();
        let button = Button::new(id)
            .ghost()
            .small()
            .tooltip(if end { "End time zone" } else { "Start time zone" })
            .label(label(chosen, at))
            .dropdown_menu(move |menu, _, _| {
                let menu = menu.min_w(px(320.0)).scrollable(true).max_h(px(420.0));
                // the zone chosen first, then every zone by offset
                let menu = menu
                    .item(PopupMenuItem::new(chosen_name.clone()).checked(true))
                    .separator();
                choices(at).into_iter().fold(menu, |menu, (zone, name)| {
                    let form = form.clone();
                    let checked = name == chosen_name;
                    let selector = format!("{id}-{}", zone.name().replace('/', "-").to_ascii_lowercase());
                    menu.item(
                        PopupMenuItem::element(move |_, _| {
                            let selector = selector.clone();
                            div().debug_selector(move || selector.clone()).child(name.clone())
                        })
                        .checked(checked)
                        .on_click(move |_, _, cx: &mut App| {
                            let _ = form.update(cx, |form, cx| form.set_zone(end, zone, cx));
                        }),
                    )
                })
            });
        selected_by(id, button)
    }

    /// The start line's date: six weeks of days from a week ago, as the start's zone shows them; the start moves to it
    /// at the same time of day there.
    pub(super) fn start_date_menu(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let slot = self.slot;
        let zone = self.zones.unwrap_or_else(Zones::desktop).start;
        let start = wall_in(slot.start, zone);
        let form = cx.entity().downgrade();
        Button::new("event-date")
            .ghost()
            .small()
            .tooltip("Start date")
            .label(start.format("%a %-m/%-d/%Y").to_string())
            .dropdown_menu(move |menu, _, _| {
                let menu = menu.min_w(px(220.0)).scrollable(true).max_h(px(420.0));
                (0..42).fold(menu, |menu, offset| {
                    let date = Local::now().date_naive() + Duration::days(offset - 7);
                    let item = slot_item(form.clone(), date.format("%a %-m/%-d/%Y").to_string(), move |slot| {
                        slot.starting(local_of(date.and_time(start.time()), zone))
                    });
                    menu.item(item.checked(date == start.date()))
                })
            })
    }

    /// The end line's date: six weeks of days from a week ago, as the end's zone shows them.
    pub(super) fn end_date_menu(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let slot = self.slot;
        let zone = self.zones.unwrap_or_else(Zones::desktop).end;
        let end = wall_in(slot.end, zone);
        let form = cx.entity().downgrade();
        let button = Button::new("event-end-date")
            .ghost()
            .small()
            .tooltip("End date")
            .label(end.format("%a %-m/%-d/%Y").to_string())
            .dropdown_menu(move |menu, _, _| {
                let menu = menu.min_w(px(220.0)).scrollable(true).max_h(px(420.0));
                (0..42).fold(menu, |menu, offset| {
                    let date = Local::now().date_naive() + Duration::days(offset - 7);
                    let item = slot_item(form.clone(), date.format("%a %-m/%-d/%Y").to_string(), move |slot| {
                        slot.ending(local_of(date.and_time(end.time()), zone))
                    });
                    menu.item(item.checked(date == end.date()))
                })
            });
        selected_by("event-end-date", button)
    }

    /// The start's time as its zone shows it: the zone's half hours of that day.
    pub(super) fn zoned_start_menu(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let cycle = clock::cycle(cx);
        let slot = self.slot;
        let zone = self.zones.unwrap_or_else(Zones::desktop).start;
        let start = wall_in(slot.start, zone);
        let form = cx.entity().downgrade();
        Button::new("event-start")
            .ghost()
            .small()
            .tooltip("Start time")
            .label(start.format(cycle.time_of_day()).to_string())
            .dropdown_menu(move |menu, _, _| {
                let menu = menu.min_w(px(160.0)).scrollable(true).max_h(px(420.0));
                model::day_times(start.date()).into_iter().fold(menu, |menu, time| {
                    let label = time.format(cycle.time_of_day()).to_string();
                    let item = slot_item(form.clone(), label, move |slot| slot.starting(local_of(time, zone)));
                    menu.item(item.checked(time == start))
                })
            })
    }

    /// The end's time as its zone shows it: the zone's half hours of the end's day.
    pub(super) fn zoned_end_menu(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let cycle = clock::cycle(cx);
        let slot = self.slot;
        let zone = self.zones.unwrap_or_else(Zones::desktop).end;
        let end = wall_in(slot.end, zone);
        let form = cx.entity().downgrade();
        Button::new("event-end")
            .ghost()
            .small()
            .tooltip("End time")
            .label(end.format(cycle.time_of_day()).to_string())
            .dropdown_menu(move |menu, _, _| {
                let menu = menu.min_w(px(160.0)).scrollable(true).max_h(px(420.0));
                model::day_times(end.date()).into_iter().fold(menu, |menu, time| {
                    let label = time.format(cycle.time_of_day()).to_string();
                    let item = slot_item(form.clone(), label, move |slot| slot.ending(local_of(time, zone)));
                    menu.item(item.checked(time == end))
                })
            })
    }

    /// The lines the globe opens: the start's date, time and zone, then the end's.
    pub(super) fn zoned_lines(&self, cx: &mut Context<Self>) -> gpui::Div {
        let muted = crate::theme::current(cx).muted_foreground;
        let caption = move |text: &'static str| {
            div()
                .w(px(44.0))
                .flex_none()
                .text_xs()
                .text_color(gpui::rgb(muted))
                .child(text)
        };
        let start = div()
            .debug_selector(|| "event-zoned-start".to_owned())
            .flex()
            .items_center()
            .gap_1()
            .child(caption("Start"))
            .child(selected_by("event-date", self.start_date_menu(cx)))
            .child(selected_by("event-start", self.zoned_start_menu(cx)))
            .child(self.zone_menu(false, cx));
        let end = div()
            .debug_selector(|| "event-zoned-end".to_owned())
            .flex()
            .items_center()
            .gap_1()
            .child(caption("End"))
            .child(self.end_date_menu(cx))
            .child(selected_by("event-end", self.zoned_end_menu(cx)))
            .child(self.zone_menu(true, cx));
        div().flex().flex_col().gap_1().child(start).child(end)
    }
}

/// The start zone's IANA name the draft is written in, when the zones shown name one other than the desktop's.
#[must_use]
pub fn draft_zone(zones: Option<Zones>) -> Option<String> {
    let zones = zones?;
    (zones.start != desktop_zone().unwrap_or(Tz::UTC)).then(|| zones.start.name().to_owned())
}

/// A slot's times as the draft writes them: the start with its zone's offset, the end with its own.
#[must_use]
pub fn zoned_slot(slot: Slot, zones: Option<Zones>) -> (ZonedTime, ZonedTime) {
    match zones {
        Some(zones) => (
            zoned_in(slot.start, Some(zones.start)),
            zoned_in(slot.end, Some(zones.start)),
        ),
        None => slot.zoned(),
    }
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use super::*;

    fn at(year: i32, month: u32, day: u32, hour: u32) -> DateTime<Utc> {
        Utc.from_utc_datetime(
            &NaiveDate::from_ymd_opt(year, month, day)
                .unwrap()
                .and_hms_opt(hour, 0, 0)
                .unwrap(),
        )
    }

    #[test]
    fn a_zone_is_named_by_its_offset_then_and_its_city() {
        let november = at(2026, 11, 3, 10);
        let july = at(2026, 7, 3, 10);
        assert_eq!(label(chrono_tz::Europe::Istanbul, november), "(UTC+03:00) Istanbul");
        assert_eq!(label(chrono_tz::Europe::London, november), "(UTC+00:00) London");
        assert_eq!(label(chrono_tz::Europe::London, july), "(UTC+01:00) London");
        assert_eq!(label(chrono_tz::America::New_York, november), "(UTC-05:00) New York");
        assert_eq!(label(chrono_tz::Asia::Kolkata, november), "(UTC+05:30) Kolkata");
        assert_eq!(label(Tz::UTC, november), "(UTC) Coordinated Universal Time");
    }

    #[test]
    fn the_choices_run_by_offset_each_name_once() {
        let zones = choices(at(2026, 11, 3, 10));
        assert!(zones.len() > 200);
        let offsets: Vec<i32> = zones
            .iter()
            .map(|(zone, _)| at(2026, 11, 3, 10).with_timezone(zone).offset().fix().local_minus_utc())
            .collect();
        assert!(offsets.windows(2).all(|pair| pair[0] <= pair[1]));
        let mut names: Vec<&String> = zones.iter().map(|(_, name)| name).collect();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count);
        // a zone and its alias (Asia/Istanbul) are listed once, by its name
        assert!(zones.iter().any(|(_, name)| name == "(UTC+03:00) Istanbul"));
        assert!(
            !zones
                .iter()
                .any(|(zone, _)| zone.name().starts_with("Etc/") || zone.name().starts_with("US/"))
        );
    }

    #[test]
    fn a_time_is_read_and_written_in_a_zone_and_back() {
        let local = NaiveDate::from_ymd_opt(2026, 11, 3)
            .unwrap()
            .and_hms_opt(13, 0, 0)
            .unwrap();
        let london = wall_in(local, chrono_tz::Europe::London);
        assert_eq!(local_of(london, chrono_tz::Europe::London), local);
        let written = zoned_in(local, Some(chrono_tz::Asia::Kolkata));
        assert_eq!(written.offset_minutes, 330);
        assert_eq!(written.ms, model::zoned(local).ms);
        assert_eq!(zoned_in(local, None), model::zoned(local));
    }

    #[test]
    fn the_draft_names_a_zone_only_when_the_start_s_isnt_the_desktop_s() {
        assert_eq!(draft_zone(None), None);
        let desktop = Zones::desktop();
        assert_eq!(draft_zone(Some(desktop)), None);
        let other = if desktop.start == chrono_tz::Pacific::Auckland {
            chrono_tz::Europe::London
        } else {
            chrono_tz::Pacific::Auckland
        };
        let chosen = Zones {
            start: other,
            end: desktop.end,
        };
        assert_eq!(draft_zone(Some(chosen)), Some(other.name().to_owned()));
        let slot = Slot::next_half_hour(Local::now().naive_local());
        let (start, end) = zoned_slot(slot, Some(chosen));
        assert_eq!(start.ms, slot.zoned().0.ms);
        assert_eq!(end.ms, slot.zoned().1.ms);
        // the service reads both times as the start zone's clock: the end carries the start zone's offset too
        let apart = Zones {
            start: other,
            end: chrono_tz::Asia::Kolkata,
        };
        assert_eq!(zoned_slot(slot, Some(apart)).1, zoned_in(slot.end, Some(other)));
    }
}
