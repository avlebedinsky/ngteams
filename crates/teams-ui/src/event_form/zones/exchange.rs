//! An event's time zone as the middle tier names it (`eventTimeZone`: `HawaiianSt`, `TurkeySt`, `UtcMinus11`): the
//! Windows zones, each named in that service's short form ("Hawaiian Standard Time" is `HawaiianSt`), seen on events
//! read 2026-11-06 and listed in full by the web client's code. Each is read as the IANA zone of the Windows zone's
//! main place (CLDR's `windowsZones`), or a zone of the same clock in a listed region where that is `Etc/` (`UTC-11`:
//! Pago Pago), so the form's zone pickers list it, and writing it back names the same Exchange zone.

use chrono_tz::Tz;

/// Each zone's middle-tier name and the IANA zone it is read as.
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

/// The IANA zone the middle tier's zone `name` is read as (its case ignored: the web client's list writes
/// `UTCPlus12`); `None` for a name it doesn't list.
#[must_use]
pub fn zone_of(name: &str) -> Option<Tz> {
    let name = name.trim();
    ZONES
        .iter()
        .find(|(known, _)| known.eq_ignore_ascii_case(name))
        .and_then(|(_, zone)| zone.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_name_reads_as_a_zone_once() {
        for (name, zone) in ZONES {
            assert!(zone.parse::<Tz>().is_ok(), "{name}: {zone}");
        }
        let mut names: Vec<String> = ZONES.iter().map(|(name, _)| name.to_ascii_lowercase()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), ZONES.len());
    }

    #[test]
    fn a_name_reads_as_its_zone_whatever_its_case() {
        assert_eq!(zone_of("HawaiianSt"), Some(chrono_tz::Pacific::Honolulu));
        assert_eq!(zone_of("TurkeySt"), Some(chrono_tz::Europe::Istanbul));
        assert_eq!(zone_of(" UTCPlus12 "), Some(chrono_tz::Pacific::Tarawa));
        assert_eq!(zone_of("Utc"), Some(Tz::UTC));
        assert_eq!(zone_of("Hawaiian Standard Time"), None);
        assert_eq!(zone_of(""), None);
    }
}
