//! Time zones as the middle tier names them on an event (`eventTimeZone`: `TurkeySt`, `UtcMinus11`): the Windows
//! zones in that service's short form, each with the IANA zone of its main place (CLDR's `windowsZones`). The
//! client reads the same names, so an event written here opens in its own zone there.

use chrono::{DateTime, FixedOffset, LocalResult, NaiveDateTime, Offset as _, TimeZone as _, Utc};
use chrono_tz::Tz;

const ZONES: [(&str, &str); 140] = [
    ("DatelineSt", "Etc/GMT+12"),
    ("UtcMinus11", "Pacific/Pago_Pago"),
    ("HawaiianSt", "Pacific/Honolulu"),
    ("AlaskanSt", "America/Anchorage"),
    ("PacificStMexico", "America/Tijuana"),
    ("PacificSt", "America/Los_Angeles"),
    ("UsMountainSt", "America/Phoenix"),
    ("MountainStMexico", "America/Mazatlan"),
    ("MountainSt", "America/Denver"),
    ("CentralAmericaSt", "America/Guatemala"),
    ("CentralSt", "America/Chicago"),
    ("CentralStMexico", "America/Mexico_City"),
    ("CanadaCentralSt", "America/Regina"),
    ("SaPacificSt", "America/Bogota"),
    ("EasternSt", "America/New_York"),
    ("UsEasternSt", "America/Indiana/Indianapolis"),
    ("VenezulaSt", "America/Caracas"),
    ("ParaguaySt", "America/Asuncion"),
    ("AtlanticSt", "America/Halifax"),
    ("CentralBrazilianSt", "America/Cuiaba"),
    ("SaWesternSt", "America/La_Paz"),
    ("PacificSaSt", "America/Santiago"),
    ("NewfoundlandSt", "America/St_Johns"),
    ("ESouthAmericaSt", "America/Sao_Paulo"),
    ("ArgentinaSt", "America/Argentina/Buenos_Aires"),
    ("SaEasternSt", "America/Cayenne"),
    ("GreenlandSt", "America/Nuuk"),
    ("MontevideoSt", "America/Montevideo"),
    ("BahiaSt", "America/Bahia"),
    ("UtcMinus02", "America/Noronha"),
    ("MidAtlanticSt", "Atlantic/South_Georgia"),
    ("AzoresSt", "Atlantic/Azores"),
    ("CapeVerdeSt", "Atlantic/Cape_Verde"),
    ("MoroccoSt", "Africa/Casablanca"),
    ("Utc", "UTC"),
    ("GmtSt", "Europe/London"),
    ("GreenwichSt", "Atlantic/Reykjavik"),
    ("WEuropeSt", "Europe/Berlin"),
    ("CentralEuropeSt", "Europe/Budapest"),
    ("RomanceSt", "Europe/Paris"),
    ("CentralEuropeanSt", "Europe/Warsaw"),
    ("WCentralAfricaSt", "Africa/Lagos"),
    ("NamibiaSt", "Africa/Windhoek"),
    ("JordanSt", "Asia/Amman"),
    ("GTBSt", "Europe/Bucharest"),
    ("MiddleEastSt", "Asia/Beirut"),
    ("EgyptSt", "Africa/Cairo"),
    ("SyriaSt", "Asia/Damascus"),
    ("EEuropeSt", "Europe/Chisinau"),
    ("SouthAfricaSt", "Africa/Johannesburg"),
    ("FLESt", "Europe/Kyiv"),
    ("TurkeySt", "Europe/Istanbul"),
    ("IsraelSt", "Asia/Jerusalem"),
    ("KaliningradSt", "Europe/Kaliningrad"),
    ("LibyaSt", "Africa/Tripoli"),
    ("ArabicSt", "Asia/Baghdad"),
    ("ArabSt", "Asia/Riyadh"),
    ("BelarusSt", "Europe/Minsk"),
    ("RussianSt", "Europe/Moscow"),
    ("EAfricaSt", "Africa/Nairobi"),
    ("IranSt", "Asia/Tehran"),
    ("ArabianSt", "Asia/Dubai"),
    ("AzerbaijanSt", "Asia/Baku"),
    ("RussiaTimeZone3", "Europe/Samara"),
    ("MauritiusSt", "Indian/Mauritius"),
    ("GeorgianSt", "Asia/Tbilisi"),
    ("CaucasusSt", "Asia/Yerevan"),
    ("AfghanistanSt", "Asia/Kabul"),
    ("WestAsiaSt", "Asia/Tashkent"),
    ("EkaterinburgSt", "Asia/Yekaterinburg"),
    ("PakistanSt", "Asia/Karachi"),
    ("IndianSt", "Asia/Kolkata"),
    ("SriLankaSt", "Asia/Colombo"),
    ("NepalSt", "Asia/Kathmandu"),
    ("CentralAsiaSt", "Asia/Bishkek"),
    ("BangladeshSt", "Asia/Dhaka"),
    ("NCentralAsiaSt", "Asia/Novosibirsk"),
    ("MyanmarSt", "Asia/Yangon"),
    ("SeAsiaSt", "Asia/Bangkok"),
    ("NAsiaSt", "Asia/Krasnoyarsk"),
    ("ChinaSt", "Asia/Shanghai"),
    ("NorthAsiaEastSt", "Asia/Irkutsk"),
    ("SingaporeSt", "Asia/Singapore"),
    ("WAustraliaSt", "Australia/Perth"),
    ("TaipeiSt", "Asia/Taipei"),
    ("UlaanbaatarSt", "Asia/Ulaanbaatar"),
    ("TokyoSt", "Asia/Tokyo"),
    ("KoreaSt", "Asia/Seoul"),
    ("YakutskSt", "Asia/Yakutsk"),
    ("CenAustraliaSt", "Australia/Adelaide"),
    ("AusCentralSt", "Australia/Darwin"),
    ("EAustraliaSt", "Australia/Brisbane"),
    ("AusEasternSt", "Australia/Sydney"),
    ("WestPacificSt", "Pacific/Port_Moresby"),
    ("TasmaniaSt", "Australia/Hobart"),
    ("MagadanSt", "Asia/Magadan"),
    ("VladivostokSt", "Asia/Vladivostok"),
    ("RussiaTimeZone10", "Asia/Srednekolymsk"),
    ("CentralPacificSt", "Pacific/Guadalcanal"),
    ("RussiaTimeZone11", "Asia/Kamchatka"),
    ("NewZealandSt", "Pacific/Auckland"),
    ("UtcPlus12", "Pacific/Tarawa"),
    ("FijiSt", "Pacific/Fiji"),
    ("KamchatkaSt", "Asia/Kamchatka"),
    ("TongaSt", "Pacific/Tongatapu"),
    ("SamoaSt", "Pacific/Apia"),
    ("LineIslandsSt", "Pacific/Kiritimati"),
    ("AleutianSt", "America/Adak"),
    ("MarquesasSt", "Pacific/Marquesas"),
    ("UtcMinus09", "Pacific/Gambier"),
    ("UtcMinus08", "Pacific/Pitcairn"),
    ("EasternStMexico", "America/Cancun"),
    ("EasterIslandSt", "Pacific/Easter"),
    ("HaitiSt", "America/Port-au-Prince"),
    ("CubaSt", "America/Havana"),
    ("TurksAndCaicosSt", "America/Grand_Turk"),
    ("MagallanesSt", "America/Punta_Arenas"),
    ("TocantinsSt", "America/Araguaina"),
    ("SaintPierreSt", "America/Miquelon"),
    ("WestBankSt", "Asia/Hebron"),
    ("SudanSt", "Africa/Khartoum"),
    ("AstrakhanSt", "Europe/Astrakhan"),
    ("SaratovSt", "Europe/Saratov"),
    ("VolgogradSt", "Europe/Volgograd"),
    ("QyzylordaSt", "Asia/Qyzylorda"),
    ("OmskSt", "Asia/Omsk"),
    ("AltaiSt", "Asia/Barnaul"),
    ("TomskSt", "Asia/Tomsk"),
    ("WestMongoliaSt", "Asia/Hovd"),
    ("TransbaikalSt", "Asia/Chita"),
    ("NorthKoreaSt", "Asia/Pyongyang"),
    ("AusCentralWestSt", "Australia/Eucla"),
    ("BougainvilleSt", "Pacific/Bougainville"),
    ("SakhalinSt", "Asia/Sakhalin"),
    ("LordHoweSt", "Australia/Lord_Howe"),
    ("NorfolkSt", "Pacific/Norfolk"),
    ("ChathamIslandsSt", "Pacific/Chatham"),
    ("UtcPlus13", "Pacific/Fakaofo"),
    ("SSudanSt", "Africa/Juba"),
    ("YukonSt", "America/Whitehorse"),
];

/// The zone an event's times are read in: a named one, or the fixed offset the event carries when its zone's name
/// isn't a listed one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Zone {
    Named(Tz),
    Fixed(FixedOffset),
}

impl Zone {
    /// The zone of an event that names `name` and carries `offset_minutes`.
    pub(super) fn of(name: Option<&str>, offset_minutes: i64) -> Self {
        name.and_then(named).map_or_else(
            || {
                let seconds = i32::try_from(offset_minutes.clamp(-1080, 1080) * 60).unwrap_or(0);
                Self::Fixed(FixedOffset::east_opt(seconds).unwrap_or_else(|| Utc.fix()))
            },
            Self::Named,
        )
    }

    /// The zone's clock at the instant `ms`.
    pub(super) fn local(self, ms: i64) -> NaiveDateTime {
        let utc = DateTime::from_timestamp_millis(ms).unwrap_or(DateTime::UNIX_EPOCH);
        match self {
            Self::Named(zone) => utc.with_timezone(&zone).naive_local(),
            Self::Fixed(offset) => utc.with_timezone(&offset).naive_local(),
        }
    }

    /// The instant the zone's clock shows `local`: the first of two when the clock goes back over it, an hour on
    /// when it is skipped.
    pub(super) fn instant(self, local: NaiveDateTime) -> i64 {
        let at = |local: NaiveDateTime| match self {
            Self::Named(zone) => first(zone.from_local_datetime(&local)),
            Self::Fixed(offset) => first(offset.from_local_datetime(&local)),
        };
        at(local)
            .or_else(|| at(local + chrono::Duration::hours(1)))
            .unwrap_or_else(|| local.and_utc().timestamp_millis())
    }
}

fn first<Z: chrono::TimeZone>(result: LocalResult<DateTime<Z>>) -> Option<i64> {
    result.earliest().map(|time| time.timestamp_millis())
}

/// The IANA zone of a middle-tier zone name, whatever its case.
fn named(name: &str) -> Option<Tz> {
    let name = name.trim();
    ZONES
        .iter()
        .find(|(known, _)| known.eq_ignore_ascii_case(name))
        .and_then(|(_, zone)| zone.parse().ok())
}

/// The middle tier's name of the IANA zone `iana` and its offset now, in minutes: the zone itself when it is a
/// listed one, else a listed zone whose clock is the same through this year.
pub(super) fn exchange(iana: &str, now: DateTime<Utc>) -> Option<(&'static str, i32)> {
    let zone: Tz = iana.trim().parse().ok()?;
    let offset = |zone: Tz, at: DateTime<Utc>| at.with_timezone(&zone).offset().fix().local_minus_utc() / 60;
    let through_the_year = |zone: Tz| -> Vec<i32> {
        (0..12)
            .map(|month| offset(zone, now + chrono::Duration::days(30 * month)))
            .collect()
    };
    let listed = |known: &str| known.parse::<Tz>().ok();
    let name = ZONES
        .iter()
        .find(|(_, known)| listed(known) == Some(zone))
        .or_else(|| {
            let clock = through_the_year(zone);
            ZONES
                .iter()
                .find(|(_, known)| listed(known).is_some_and(|known| through_the_year(known) == clock))
        })
        .map(|(name, _)| *name)?;
    Some((name, offset(zone, now)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_zone_reads_and_is_named_once() {
        let mut names: Vec<String> = ZONES.iter().map(|(name, _)| name.to_ascii_lowercase()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), ZONES.len());
        for (name, zone) in ZONES {
            assert!(zone.parse::<Tz>().is_ok(), "{name}: {zone}");
        }
    }

    #[test]
    fn an_iana_zone_is_named_as_the_middle_tier_names_it() {
        let winter = DateTime::parse_from_rfc3339("2027-01-15T12:00:00Z").unwrap().to_utc();
        assert_eq!(exchange("Europe/Istanbul", winter), Some(("TurkeySt", 180)));
        assert_eq!(exchange("Pacific/Honolulu", winter), Some(("HawaiianSt", -600)));
        assert_eq!(exchange("UTC", winter), Some(("Utc", 0)));
        // a zone that isn't a Windows zone's main place is named by one with the same clock
        assert_eq!(exchange("Europe/Vilnius", winter).map(|(_, offset)| offset), Some(120));
        assert_eq!(exchange("Europe/Vilnius", winter), exchange("Europe/Bucharest", winter));
        assert_eq!(exchange("Mars/Olympus", winter), None);
    }

    #[test]
    fn a_zone_s_clock_follows_its_daylight_time() {
        let berlin = Zone::of(Some("WEuropeSt"), 0);
        let summer = DateTime::parse_from_rfc3339("2027-07-01T08:00:00Z")
            .unwrap()
            .timestamp_millis();
        assert_eq!(berlin.local(summer).to_string(), "2027-07-01 10:00:00");
        assert_eq!(berlin.instant(berlin.local(summer)), summer);
        // 02:30 on the night the clock skips to 03:00 is read an hour on
        let skipped = NaiveDateTime::parse_from_str("2027-03-28 02:30:00", "%Y-%m-%d %H:%M:%S").unwrap();
        assert_eq!(berlin.local(berlin.instant(skipped)).to_string(), "2027-03-28 03:30:00");
        // a zone the list doesn't name keeps the offset the event carries
        let fixed = Zone::of(Some("Somewhere"), 330);
        assert_eq!(fixed.local(summer).to_string(), "2027-07-01 13:30:00");
        assert_eq!(fixed.instant(fixed.local(summer)), summer);
        assert_eq!(Zone::of(None, 99_999), Zone::of(None, 1080));
    }
}
