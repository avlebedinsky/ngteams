//! The scheduling form's time zones, as Teams' form shows them behind its globe: the start's date, time and time zone
//! on one line, the end's date, time and time zone under it. The event's slot stays in the desktop's time (the day
//! beside the form and the Scheduler draw it there); the zones only say in which zone its start and end are read and
//! picked. The middle tier's event has one zone, the start's (its Exchange name read by the engine,
//! `EventDraft::zone`), and reads both its times as that zone's clock whatever offset they carry (seen live
//! 2026-11-06: an end written with another zone's offset was taken as the start zone's time), so both are written in
//! the start's zone: an end picked in another zone is still the same moment.
//!
//! The zones listed are the IANA zones of the world's regions, each as "(UTC+03:00) Istanbul" at the event's start,
//! by offset then name. A picker is Teams' combobox: typing searches the zones by city (Teams' placeholder, "Search
//! for a city"); its rows are built once for the start's day and drawn only as far as they are scrolled to, so it opens
//! at once even with its four hundred rows.
//!
//! A series repeats in the start's zone, as Outlook's form writes it (its weekday, its first and last days are the
//! start zone's, seen 2026-11-06: a start at 10:00 Honolulu on a Friday is "Occurs every Friday … (UTC-10)", whatever
//! day the desktop's clock is on): [`series_day`], [`midnight`].

use chrono::{DateTime, Duration, Local, NaiveDate, NaiveDateTime, NaiveTime, Offset as _, TimeZone as _, Utc};
use chrono_tz::OffsetComponents as _;
pub use chrono_tz::Tz;
use gpui::{
    App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, Subscription, Window, div, px,
};
use gpui_component::Sizable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::menu::DropdownMenu as _;
use gpui_component::select::{SearchableVec, Select, SelectEvent, SelectItem, SelectState};
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

/// The choices with `chosen` among them: it takes the place of the zone listed by its name (an alias, such as
/// Asia/Istanbul for Istanbul), or joins the list by its offset when none is.
#[must_use]
pub fn choices_with(mut listed: Vec<(Tz, String)>, chosen: Tz, at: DateTime<Utc>) -> Vec<(Tz, String)> {
    if listed.iter().any(|(zone, _)| *zone == chosen) {
        return listed;
    }
    let name = label(chosen, at);
    if let Some(same) = listed.iter_mut().find(|(_, listed)| *listed == name) {
        same.0 = chosen;
        return listed;
    }
    let offset = |zone: &Tz| at.with_timezone(zone).offset().fix().local_minus_utc();
    let place = listed
        .iter()
        .position(|(zone, listed)| (offset(zone), listed.as_str()) > (offset(&chosen), name.as_str()))
        .unwrap_or(listed.len());
    listed.insert(place, (chosen, name));
    listed
}

/// A row of a zone picker: the zone, its name, and the row's test selector (`event-start-zone-pacific-honolulu`).
#[derive(Clone, Debug)]
pub struct ZoneChoice {
    zone: Tz,
    label: SharedString,
    selector: SharedString,
}

impl ZoneChoice {
    fn new(id: &str, zone: Tz, label: String) -> Self {
        let selector = format!("{id}-{}", zone.name().replace('/', "-").to_ascii_lowercase());
        Self {
            zone,
            label: label.into(),
            selector: selector.into(),
        }
    }
}

impl SelectItem for ZoneChoice {
    type Value = Tz;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    fn render(&self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let selector = self.selector.clone();
        div()
            .debug_selector(move || selector.to_string())
            .child(self.label.clone())
    }

    fn value(&self) -> &Tz {
        &self.zone
    }

    /// A city, a region or an offset: "hono", "pacific", "+05:30".
    fn matches(&self, query: &str) -> bool {
        let query = query.trim().to_lowercase();
        self.label.to_lowercase().contains(&query) || self.zone.name().replace('_', " ").to_lowercase().contains(&query)
    }
}

/// A zone picker's state.
pub type ZoneSelect = SelectState<SearchableVec<ZoneChoice>>;

/// The start's and the end's zone pickers, their rows named for the offsets on `day` (the start's, in UTC).
pub struct ZonePickers {
    start: Entity<ZoneSelect>,
    end: Entity<ZoneSelect>,
    day: NaiveDate,
    _confirmed: [Subscription; 2],
}

/// A picker's rows: the zones at `at`, `chosen` among them.
fn zone_rows(id: &str, listed: &[(Tz, String)], chosen: Tz, at: DateTime<Utc>) -> SearchableVec<ZoneChoice> {
    let rows: Vec<ZoneChoice> = choices_with(listed.to_vec(), chosen, at)
        .into_iter()
        .map(|(zone, label)| ZoneChoice::new(id, zone, label))
        .collect();
    SearchableVec::new(rows)
}

/// The id of the start's picker (`end` false) or the end's.
fn picker_id(end: bool) -> &'static str {
    if end { "event-end-zone" } else { "event-start-zone" }
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
        let from = self.series_day();
        self.zones = shown.then(Zones::desktop);
        self.follow_series_day(from);
        cx.notify();
    }

    /// The start's (`end` false) or the end's time zone changed to `zone`; a series follows the start's day there.
    pub fn set_zone(&mut self, end: bool, zone: Tz, cx: &mut Context<Self>) {
        let from = self.series_day();
        let mut zones = self.zones.unwrap_or_else(Zones::desktop);
        if end {
            zones.end = zone;
        } else {
            zones.start = zone;
        }
        self.zones = Some(zones);
        self.follow_series_day(from);
        cx.notify();
    }

    /// The day a series starts on: the start's date in the zone it repeats in ([`series_day`]).
    #[must_use]
    pub fn series_day(&self) -> NaiveDate {
        series_day(self.slot, self.zones)
    }

    /// The zone a series repeats in, when it isn't the desktop's ([`series_zone`]).
    #[must_use]
    pub fn series_zone(&self) -> Option<Tz> {
        series_zone(self.zones, self.slot.all_day)
    }

    /// A series' preset follows its first day, moved from `from` (its end dropped when it is now before it).
    pub(super) fn follow_series_day(&mut self, from: NaiveDate) {
        let to = self.series_day();
        if let Some(repeat) = &self.repeat
            && from != to
        {
            self.repeat = Some(repeat.moved(from, to));
        }
    }

    /// The two zone pickers, made the first time the globe shows them, their rows named again when the start moves
    /// to another day, each showing its zone.
    fn zone_pickers(&mut self, window: &mut Window, cx: &mut Context<Self>) -> [Entity<ZoneSelect>; 2] {
        let zones = self.zones.unwrap_or_else(Zones::desktop);
        let at = instant(self.slot.start);
        let day = at.date_naive();
        let pickers = match self.zone_pickers.take() {
            Some(pickers) if pickers.day == day => pickers,
            Some(pickers) => {
                let listed = choices(at);
                for (end, state, chosen) in [(false, &pickers.start, zones.start), (true, &pickers.end, zones.end)] {
                    let rows = zone_rows(picker_id(end), &listed, chosen, at);
                    state.update(cx, |state, cx| state.set_items(rows, window, cx));
                }
                ZonePickers { day, ..pickers }
            }
            None => Self::new_zone_pickers(zones, at, window, cx),
        };
        for (end, state, chosen) in [(false, &pickers.start, zones.start), (true, &pickers.end, zones.end)] {
            if state.read(cx).selected_value() == Some(&chosen) {
                continue;
            }
            // a zone the rows don't list (set by the tour, or the desktop's alias) joins them first
            state.update(cx, |state, cx| {
                state.set_selected_value(&chosen, window, cx);
                if state.selected_value() != Some(&chosen) {
                    state.set_items(zone_rows(picker_id(end), &choices(at), chosen, at), window, cx);
                    state.set_selected_value(&chosen, window, cx);
                }
            });
        }
        let states = [pickers.start.clone(), pickers.end.clone()];
        self.zone_pickers = Some(pickers);
        states
    }

    fn new_zone_pickers(zones: Zones, at: DateTime<Utc>, window: &mut Window, cx: &mut Context<Self>) -> ZonePickers {
        let listed = choices(at);
        let mut picker = |end: bool, chosen: Tz| {
            let rows = zone_rows(picker_id(end), &listed, chosen, at);
            let state = cx.new(|cx| {
                let mut state = SelectState::new(rows, None, window, cx).searchable(true);
                state.set_selected_value(&chosen, window, cx);
                state
            });
            let confirmed = cx.subscribe_in(
                &state,
                window,
                move |form: &mut Self, _, event: &SelectEvent<SearchableVec<ZoneChoice>>, _, cx| {
                    if let SelectEvent::Confirm(Some(zone)) = event {
                        form.set_zone(end, *zone, cx);
                    }
                },
            );
            (state, confirmed)
        };
        let (start, start_confirmed) = picker(false, zones.start);
        let (end, end_confirmed) = picker(true, zones.end);
        ZonePickers {
            start,
            end,
            day: at.date_naive(),
            _confirmed: [start_confirmed, end_confirmed],
        }
    }

    /// The start line's zone picker, or the end line's.
    fn zone_menu(state: &Entity<ZoneSelect>, end: bool) -> gpui::Div {
        let id = picker_id(end);
        selected_by(
            id,
            div().w(px(250.0)).child(
                Select::new(state)
                    .id(id)
                    .small()
                    .menu_width(px(320.0))
                    .menu_max_h(px(420.0))
                    .search_placeholder("Search for a city")
                    .accessibility_label(if end { "End time zone" } else { "Start time zone" }),
            ),
        )
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
    pub(super) fn zoned_lines(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::Div {
        let [start_zone, end_zone] = self.zone_pickers(window, cx);
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
            .child(Self::zone_menu(&start_zone, false));
        let end = div()
            .debug_selector(|| "event-zoned-end".to_owned())
            .flex()
            .items_center()
            .gap_1()
            .child(caption("End"))
            .child(self.end_date_menu(cx))
            .child(selected_by("event-end", self.zoned_end_menu(cx)))
            .child(Self::zone_menu(&end_zone, true));
        div().flex().flex_col().gap_1().child(start).child(end)
    }
}

/// The start's zone the draft is written in, when the zones shown name one other than the desktop's.
#[must_use]
pub fn start_zone(zones: Option<Zones>) -> Option<Tz> {
    let zones = zones?;
    (zones.start != desktop_zone().unwrap_or(Tz::UTC)).then_some(zones.start)
}

/// The start zone's IANA name the draft is written in, when the zones shown name one other than the desktop's.
#[must_use]
pub fn draft_zone(zones: Option<Zones>) -> Option<String> {
    start_zone(zones).map(|zone| zone.name().to_owned())
}

/// The zone a series repeats in: the start's ([`start_zone`]), unless the event takes whole days (whose days are the
/// desktop's: the globe is hidden then).
#[must_use]
pub fn series_zone(zones: Option<Zones>, all_day: bool) -> Option<Tz> {
    start_zone(zones.filter(|_| !all_day))
}

/// The day a series starting at `slot` starts on: its start's date in the zone it repeats in, which gives its
/// weekday, its week and its date of the month.
#[must_use]
pub fn series_day(slot: Slot, zones: Option<Zones>) -> NaiveDate {
    series_zone(zones, slot.all_day).map_or(slot.start.date(), |zone| wall_in(slot.start, zone).date())
}

/// `date`'s midnight as `zone` writes it, with its offset then (the desktop's without a zone): a series' first and
/// last days.
#[must_use]
pub fn midnight(date: NaiveDate, zone: Option<Tz>) -> ZonedTime {
    let wall = date.and_time(NaiveTime::MIN);
    let Some(zone) = zone else {
        return model::zoned(wall);
    };
    // a midnight the zone's clock skips is the first moment of that day
    zone.from_local_datetime(&wall)
        .earliest()
        .or_else(|| zone.from_local_datetime(&(wall + Duration::hours(1))).earliest())
        .map_or_else(
            || model::zoned(wall),
            |at| ZonedTime {
                ms: at.timestamp_millis(),
                offset_minutes: at.offset().fix().local_minus_utc() / 60,
            },
        )
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

    /// A zone far from the desktop's, and a start on 2026-11-10 (a Tuesday) whose day there is another.
    fn far_start() -> (Tz, Slot, NaiveDate) {
        let desktop = Zones::desktop().start;
        let day = NaiveDate::from_ymd_opt(2026, 11, 10).unwrap();
        [chrono_tz::Pacific::Honolulu, chrono_tz::Pacific::Kiritimati]
            .into_iter()
            .filter(|zone| *zone != desktop)
            .find_map(|zone| {
                (0..24).find_map(|hour| {
                    let start = day.and_hms_opt(hour, 0, 0).unwrap();
                    let there = wall_in(start, zone).date();
                    (there != day).then(|| (zone, Slot::new(start, start + Duration::hours(1)), there))
                })
            })
            .unwrap()
    }

    #[test]
    fn a_series_starts_on_the_start_zone_s_day() {
        let (far, slot, there) = far_start();
        let desktop = Zones::desktop();
        assert_eq!(series_day(slot, None), slot.start.date());
        assert_eq!(series_day(slot, Some(desktop)), slot.start.date());
        let zones = Zones {
            start: far,
            end: desktop.end,
        };
        assert_eq!(series_zone(Some(zones), false), Some(far));
        assert_eq!(series_day(slot, Some(zones)), there);
        // an all-day event's days stay the desktop's
        assert_eq!(series_zone(Some(zones), true), None);
        assert_eq!(series_day(slot.with_all_day(true), Some(zones)), slot.start.date());
    }

    #[test]
    fn a_series_day_is_written_as_its_zone_s_midnight() {
        let day = NaiveDate::from_ymd_opt(2026, 11, 9).unwrap();
        let honolulu = midnight(day, Some(chrono_tz::Pacific::Honolulu));
        assert_eq!(honolulu.offset_minutes, -600);
        assert_eq!(
            honolulu.ms,
            Utc.from_utc_datetime(&day.and_hms_opt(10, 0, 0).unwrap())
                .timestamp_millis()
        );
        assert_eq!(midnight(day, None), model::zoned(day.and_time(NaiveTime::MIN)));
        // Santiago's clock skips its midnight on 2026-09-06: the day starts at 01:00 there
        let skipped = NaiveDate::from_ymd_opt(2026, 9, 6).unwrap();
        let santiago = midnight(skipped, Some(chrono_tz::America::Santiago));
        let shifted = santiago.ms + i64::from(santiago.offset_minutes) * 60_000;
        assert_eq!(DateTime::from_timestamp_millis(shifted).unwrap().date_naive(), skipped);
    }

    #[test]
    fn a_chosen_zone_the_list_lacks_takes_its_namesake_s_place_or_joins_by_offset() {
        let november = at(2026, 11, 3, 10);
        let listed = choices(november);
        let istanbul = listed
            .iter()
            .find(|(_, name)| name == "(UTC+03:00) Istanbul")
            .map(|(zone, _)| *zone)
            .unwrap();
        let alias = if istanbul == chrono_tz::Asia::Istanbul {
            chrono_tz::Europe::Istanbul
        } else {
            chrono_tz::Asia::Istanbul
        };
        let with_alias = choices_with(listed.clone(), alias, november);
        assert_eq!(with_alias.len(), listed.len());
        assert!(with_alias.contains(&(alias, "(UTC+03:00) Istanbul".to_owned())));
        let eastern = chrono_tz::US::Eastern;
        let with_eastern = choices_with(listed.clone(), eastern, november);
        assert_eq!(with_eastern.len(), listed.len() + 1);
        let offsets: Vec<i32> = with_eastern
            .iter()
            .map(|(zone, _)| november.with_timezone(zone).offset().fix().local_minus_utc())
            .collect();
        assert!(offsets.windows(2).all(|pair| pair[0] <= pair[1]));
        assert_eq!(choices_with(listed.clone(), istanbul, november), listed);
    }

    #[test]
    fn a_zone_row_is_found_by_its_city_its_region_or_its_offset() {
        let row = ZoneChoice::new(
            "event-start-zone",
            chrono_tz::America::New_York,
            "(UTC-05:00) New York".to_owned(),
        );
        assert_eq!(row.selector.as_ref(), "event-start-zone-america-new_york");
        assert_eq!(row.value(), &chrono_tz::America::New_York);
        assert_eq!(row.title().as_ref(), "(UTC-05:00) New York");
        for query in ["new y", "NEW YORK", "america", "-05:00", " york "] {
            assert!(row.matches(query), "{query}");
        }
        assert!(!row.matches("honolulu"));
    }
}
