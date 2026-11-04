//! The time zones Teams' Time zone field offers: Windows' zone ids (what a recurrence's `timeZone` takes) and the
//! names Teams shows, in its order. The local zone is the one whose standard offset it has and whose name holds the
//! system zone's city, else the first with that offset.

/// (Windows zone id, name shown).
pub(super) const ZONES: [(&str, &str); 136] = [
    ("Dateline Standard Time", "(UTC-12:00) International Date Line West"),
    ("UTC-11", "(UTC-11:00) Coordinated Universal Time-11"),
    ("Aleutian Standard Time", "(UTC-10:00) Aleutian Islands"),
    ("Hawaiian Standard Time", "(UTC-10:00) Hawaii"),
    ("Marquesas Standard Time", "(UTC-09:30) Marquesas Islands"),
    ("Alaskan Standard Time", "(UTC-09:00) Alaska"),
    ("UTC-09", "(UTC-09:00) Coordinated Universal Time-09"),
    ("Pacific Standard Time (Mexico)", "(UTC-08:00) Baja California"),
    ("UTC-08", "(UTC-08:00) Coordinated Universal Time-08"),
    ("Pacific Standard Time", "(UTC-08:00) Pacific Time (US & Canada)"),
    ("US Mountain Standard Time", "(UTC-07:00) Arizona"),
    (
        "Mountain Standard Time (Mexico)",
        "(UTC-07:00) Chihuahua, La Paz, Mazatlan",
    ),
    ("Mountain Standard Time", "(UTC-07:00) Mountain Time (US & Canada)"),
    ("Central America Standard Time", "(UTC-06:00) Central America"),
    ("Central Standard Time", "(UTC-06:00) Central Time (US & Canada)"),
    ("Easter Island Standard Time", "(UTC-06:00) Easter Island"),
    (
        "Central Standard Time (Mexico)",
        "(UTC-06:00) Guadalajara, Mexico City, Monterrey",
    ),
    ("Canada Central Standard Time", "(UTC-06:00) Saskatchewan"),
    (
        "SA Pacific Standard Time",
        "(UTC-05:00) Bogota, Lima, Quito, Rio Branco",
    ),
    ("Eastern Standard Time (Mexico)", "(UTC-05:00) Chetumal"),
    ("Eastern Standard Time", "(UTC-05:00) Eastern Time (US & Canada)"),
    ("Haiti Standard Time", "(UTC-05:00) Haiti"),
    ("Cuba Standard Time", "(UTC-05:00) Havana"),
    ("US Eastern Standard Time", "(UTC-05:00) Indiana (East)"),
    ("Paraguay Standard Time", "(UTC-04:00) Asuncion"),
    ("Atlantic Standard Time", "(UTC-04:00) Atlantic Time (Canada)"),
    ("Venezuela Standard Time", "(UTC-04:00) Caracas"),
    ("Central Brazilian Standard Time", "(UTC-04:00) Cuiaba"),
    (
        "SA Western Standard Time",
        "(UTC-04:00) Georgetown, La Paz, Manaus, San Juan",
    ),
    ("Pacific SA Standard Time", "(UTC-04:00) Santiago"),
    ("Turks And Caicos Standard Time", "(UTC-04:00) Turks and Caicos"),
    ("Newfoundland Standard Time", "(UTC-03:30) Newfoundland"),
    ("Tocantins Standard Time", "(UTC-03:00) Araguaina"),
    ("E. South America Standard Time", "(UTC-03:00) Brasilia"),
    ("SA Eastern Standard Time", "(UTC-03:00) Cayenne, Fortaleza"),
    ("Argentina Standard Time", "(UTC-03:00) City of Buenos Aires"),
    ("Greenland Standard Time", "(UTC-03:00) Greenland"),
    ("Montevideo Standard Time", "(UTC-03:00) Montevideo"),
    ("Magallanes Standard Time", "(UTC-03:00) Punta Arenas"),
    ("Saint Pierre Standard Time", "(UTC-03:00) Saint Pierre and Miquelon"),
    ("Bahia Standard Time", "(UTC-03:00) Salvador"),
    ("UTC-02", "(UTC-02:00) Coordinated Universal Time-02"),
    ("Mid-Atlantic Standard Time", "(UTC-02:00) Mid-Atlantic - Old"),
    ("Azores Standard Time", "(UTC-01:00) Azores"),
    ("Cape Verde Standard Time", "(UTC-01:00) Cabo Verde Is."),
    ("UTC", "(UTC) Coordinated Universal Time"),
    ("Morocco Standard Time", "(UTC+00:00) Casablanca"),
    ("GMT Standard Time", "(UTC+00:00) Dublin, Edinburgh, Lisbon, London"),
    ("Greenwich Standard Time", "(UTC+00:00) Monrovia, Reykjavik"),
    (
        "W. Europe Standard Time",
        "(UTC+01:00) Amsterdam, Berlin, Bern, Rome, Stockholm, Vienna",
    ),
    (
        "Central Europe Standard Time",
        "(UTC+01:00) Belgrade, Bratislava, Budapest, Ljubljana, Prague",
    ),
    (
        "Romance Standard Time",
        "(UTC+01:00) Brussels, Copenhagen, Madrid, Paris",
    ),
    (
        "Central European Standard Time",
        "(UTC+01:00) Sarajevo, Skopje, Warsaw, Zagreb",
    ),
    ("W. Central Africa Standard Time", "(UTC+01:00) West Central Africa"),
    ("Namibia Standard Time", "(UTC+01:00) Windhoek"),
    ("Jordan Standard Time", "(UTC+02:00) Amman"),
    ("GTB Standard Time", "(UTC+02:00) Athens, Bucharest"),
    ("Middle East Standard Time", "(UTC+02:00) Beirut"),
    ("Egypt Standard Time", "(UTC+02:00) Cairo"),
    ("E. Europe Standard Time", "(UTC+02:00) Chisinau"),
    ("Syria Standard Time", "(UTC+02:00) Damascus"),
    ("West Bank Standard Time", "(UTC+02:00) Gaza, Hebron"),
    ("South Africa Standard Time", "(UTC+02:00) Harare, Pretoria"),
    (
        "FLE Standard Time",
        "(UTC+02:00) Helsinki, Kyiv, Riga, Sofia, Tallinn, Vilnius",
    ),
    ("Israel Standard Time", "(UTC+02:00) Jerusalem"),
    ("Kaliningrad Standard Time", "(UTC+02:00) Kaliningrad"),
    ("Libya Standard Time", "(UTC+02:00) Tripoli"),
    ("Arabic Standard Time", "(UTC+03:00) Baghdad"),
    ("Turkey Standard Time", "(UTC+03:00) Istanbul"),
    ("Arab Standard Time", "(UTC+03:00) Kuwait, Riyadh"),
    ("Belarus Standard Time", "(UTC+03:00) Minsk"),
    ("Russian Standard Time", "(UTC+03:00) Moscow, St. Petersburg"),
    ("E. Africa Standard Time", "(UTC+03:00) Nairobi"),
    ("Iran Standard Time", "(UTC+03:30) Tehran"),
    ("Arabian Standard Time", "(UTC+04:00) Abu Dhabi, Muscat"),
    ("Astrakhan Standard Time", "(UTC+04:00) Astrakhan, Ulyanovsk"),
    ("Azerbaijan Standard Time", "(UTC+04:00) Baku"),
    ("Russia Time Zone 3", "(UTC+04:00) Izhevsk, Samara"),
    ("Mauritius Standard Time", "(UTC+04:00) Port Louis"),
    ("Saratov Standard Time", "(UTC+04:00) Saratov"),
    ("Georgian Standard Time", "(UTC+04:00) Tbilisi"),
    ("Volgograd Standard Time", "(UTC+04:00) Volgograd"),
    ("Caucasus Standard Time", "(UTC+04:00) Yerevan"),
    ("Afghanistan Standard Time", "(UTC+04:30) Kabul"),
    ("West Asia Standard Time", "(UTC+05:00) Ashgabat, Tashkent"),
    ("Ekaterinburg Standard Time", "(UTC+05:00) Ekaterinburg"),
    ("Pakistan Standard Time", "(UTC+05:00) Islamabad, Karachi"),
    ("India Standard Time", "(UTC+05:30) Chennai, Kolkata, Mumbai, New Delhi"),
    ("Sri Lanka Standard Time", "(UTC+05:30) Sri Jayawardenepura"),
    ("Nepal Standard Time", "(UTC+05:45) Kathmandu"),
    ("Central Asia Standard Time", "(UTC+06:00) Astana"),
    ("Bangladesh Standard Time", "(UTC+06:00) Dhaka"),
    ("Omsk Standard Time", "(UTC+06:00) Omsk"),
    ("Myanmar Standard Time", "(UTC+06:30) Yangon (Rangoon)"),
    ("SE Asia Standard Time", "(UTC+07:00) Bangkok, Hanoi, Jakarta"),
    ("Altai Standard Time", "(UTC+07:00) Barnaul, Gorno-Altaysk"),
    ("W. Mongolia Standard Time", "(UTC+07:00) Hovd"),
    ("North Asia Standard Time", "(UTC+07:00) Krasnoyarsk"),
    ("N. Central Asia Standard Time", "(UTC+07:00) Novosibirsk"),
    ("Tomsk Standard Time", "(UTC+07:00) Tomsk"),
    (
        "China Standard Time",
        "(UTC+08:00) Beijing, Chongqing, Hong Kong, Urumqi",
    ),
    ("North Asia East Standard Time", "(UTC+08:00) Irkutsk"),
    ("Singapore Standard Time", "(UTC+08:00) Kuala Lumpur, Singapore"),
    ("W. Australia Standard Time", "(UTC+08:00) Perth"),
    ("Taipei Standard Time", "(UTC+08:00) Taipei"),
    ("Ulaanbaatar Standard Time", "(UTC+08:00) Ulaanbaatar"),
    ("North Korea Standard Time", "(UTC+08:30) Pyongyang"),
    ("Aus Central W. Standard Time", "(UTC+08:45) Eucla"),
    ("Transbaikal Standard Time", "(UTC+09:00) Chita"),
    ("Tokyo Standard Time", "(UTC+09:00) Osaka, Sapporo, Tokyo"),
    ("Korea Standard Time", "(UTC+09:00) Seoul"),
    ("Yakutsk Standard Time", "(UTC+09:00) Yakutsk"),
    ("Cen. Australia Standard Time", "(UTC+09:30) Adelaide"),
    ("AUS Central Standard Time", "(UTC+09:30) Darwin"),
    ("E. Australia Standard Time", "(UTC+10:00) Brisbane"),
    ("AUS Eastern Standard Time", "(UTC+10:00) Canberra, Melbourne, Sydney"),
    ("West Pacific Standard Time", "(UTC+10:00) Guam, Port Moresby"),
    ("Tasmania Standard Time", "(UTC+10:00) Hobart"),
    ("Vladivostok Standard Time", "(UTC+10:00) Vladivostok"),
    ("Lord Howe Standard Time", "(UTC+10:30) Lord Howe Island"),
    ("Bougainville Standard Time", "(UTC+11:00) Bougainville Island"),
    ("Russia Time Zone 10", "(UTC+11:00) Chokurdakh"),
    ("Magadan Standard Time", "(UTC+11:00) Magadan"),
    ("Norfolk Standard Time", "(UTC+11:00) Norfolk Island"),
    ("Sakhalin Standard Time", "(UTC+11:00) Sakhalin"),
    (
        "Central Pacific Standard Time",
        "(UTC+11:00) Solomon Is., New Caledonia",
    ),
    ("Russia Time Zone 11", "(UTC+12:00) Anadyr, Petropavlovsk-Kamchatsky"),
    ("New Zealand Standard Time", "(UTC+12:00) Auckland, Wellington"),
    ("UTC+12", "(UTC+12:00) Coordinated Universal Time+12"),
    ("Fiji Standard Time", "(UTC+12:00) Fiji"),
    ("Kamchatka Standard Time", "(UTC+12:00) Petropavlovsk-Kamchatsky - Old"),
    ("Chatham Islands Standard Time", "(UTC+12:45) Chatham Islands"),
    ("UTC+13", "(UTC+13:00) Coordinated Universal Time+13"),
    ("Tonga Standard Time", "(UTC+13:00) Nuku'alofa"),
    ("Samoa Standard Time", "(UTC+13:00) Samoa"),
    ("Line Islands Standard Time", "(UTC+14:00) Kiritimati Island"),
];

/// Each Windows zone's own tz database zone (CLDR's `windowsZones`, territory 001) and a few big cities' zones whose
/// city no name shows: (tz database zone, Windows zone id).
pub(super) const TZDB: [(&str, &str); 144] = [
    ("Etc/GMT+12", "Dateline Standard Time"),
    ("Etc/GMT+11", "UTC-11"),
    ("America/Adak", "Aleutian Standard Time"),
    ("Pacific/Honolulu", "Hawaiian Standard Time"),
    ("Pacific/Marquesas", "Marquesas Standard Time"),
    ("America/Anchorage", "Alaskan Standard Time"),
    ("Etc/GMT+9", "UTC-09"),
    ("America/Tijuana", "Pacific Standard Time (Mexico)"),
    ("Etc/GMT+8", "UTC-08"),
    ("America/Los_Angeles", "Pacific Standard Time"),
    ("America/Vancouver", "Pacific Standard Time"),
    ("America/Phoenix", "US Mountain Standard Time"),
    ("America/Mazatlan", "Mountain Standard Time (Mexico)"),
    ("America/Denver", "Mountain Standard Time"),
    ("America/Edmonton", "Mountain Standard Time"),
    ("America/Boise", "Mountain Standard Time"),
    ("America/Guatemala", "Central America Standard Time"),
    ("America/Chicago", "Central Standard Time"),
    ("America/Winnipeg", "Central Standard Time"),
    ("Pacific/Easter", "Easter Island Standard Time"),
    ("America/Mexico_City", "Central Standard Time (Mexico)"),
    ("America/Monterrey", "Central Standard Time (Mexico)"),
    ("America/Regina", "Canada Central Standard Time"),
    ("America/Bogota", "SA Pacific Standard Time"),
    ("America/Lima", "SA Pacific Standard Time"),
    ("America/Cancun", "Eastern Standard Time (Mexico)"),
    ("America/New_York", "Eastern Standard Time"),
    ("America/Toronto", "Eastern Standard Time"),
    ("America/Detroit", "Eastern Standard Time"),
    ("America/Port-au-Prince", "Haiti Standard Time"),
    ("America/Havana", "Cuba Standard Time"),
    ("America/Indianapolis", "US Eastern Standard Time"),
    ("America/Indiana/Indianapolis", "US Eastern Standard Time"),
    ("America/Asuncion", "Paraguay Standard Time"),
    ("America/Halifax", "Atlantic Standard Time"),
    ("America/Caracas", "Venezuela Standard Time"),
    ("America/Cuiaba", "Central Brazilian Standard Time"),
    ("America/La_Paz", "SA Western Standard Time"),
    ("America/Santiago", "Pacific SA Standard Time"),
    ("America/Grand_Turk", "Turks And Caicos Standard Time"),
    ("America/St_Johns", "Newfoundland Standard Time"),
    ("America/Araguaina", "Tocantins Standard Time"),
    ("America/Sao_Paulo", "E. South America Standard Time"),
    ("America/Cayenne", "SA Eastern Standard Time"),
    ("America/Buenos_Aires", "Argentina Standard Time"),
    ("America/Argentina/Buenos_Aires", "Argentina Standard Time"),
    ("America/Godthab", "Greenland Standard Time"),
    ("America/Nuuk", "Greenland Standard Time"),
    ("America/Montevideo", "Montevideo Standard Time"),
    ("America/Punta_Arenas", "Magallanes Standard Time"),
    ("America/Miquelon", "Saint Pierre Standard Time"),
    ("America/Bahia", "Bahia Standard Time"),
    ("Etc/GMT+2", "UTC-02"),
    ("Atlantic/Azores", "Azores Standard Time"),
    ("Atlantic/Cape_Verde", "Cape Verde Standard Time"),
    ("Etc/UTC", "UTC"),
    ("Etc/GMT", "UTC"),
    ("UTC", "UTC"),
    ("Africa/Casablanca", "Morocco Standard Time"),
    ("Europe/London", "GMT Standard Time"),
    ("Europe/Dublin", "GMT Standard Time"),
    ("Europe/Lisbon", "GMT Standard Time"),
    ("Atlantic/Reykjavik", "Greenwich Standard Time"),
    ("Europe/Berlin", "W. Europe Standard Time"),
    ("Europe/Amsterdam", "W. Europe Standard Time"),
    ("Europe/Zurich", "W. Europe Standard Time"),
    ("Europe/Budapest", "Central Europe Standard Time"),
    ("Europe/Prague", "Central Europe Standard Time"),
    ("Europe/Paris", "Romance Standard Time"),
    ("Europe/Brussels", "Romance Standard Time"),
    ("Europe/Madrid", "Romance Standard Time"),
    ("Europe/Warsaw", "Central European Standard Time"),
    ("Africa/Lagos", "W. Central Africa Standard Time"),
    ("Africa/Windhoek", "Namibia Standard Time"),
    ("Asia/Amman", "Jordan Standard Time"),
    ("Europe/Bucharest", "GTB Standard Time"),
    ("Europe/Athens", "GTB Standard Time"),
    ("Asia/Beirut", "Middle East Standard Time"),
    ("Africa/Cairo", "Egypt Standard Time"),
    ("Europe/Chisinau", "E. Europe Standard Time"),
    ("Asia/Damascus", "Syria Standard Time"),
    ("Asia/Hebron", "West Bank Standard Time"),
    ("Africa/Johannesburg", "South Africa Standard Time"),
    ("Europe/Kiev", "FLE Standard Time"),
    ("Europe/Kyiv", "FLE Standard Time"),
    ("Europe/Helsinki", "FLE Standard Time"),
    ("Asia/Jerusalem", "Israel Standard Time"),
    ("Europe/Kaliningrad", "Kaliningrad Standard Time"),
    ("Africa/Tripoli", "Libya Standard Time"),
    ("Asia/Baghdad", "Arabic Standard Time"),
    ("Europe/Istanbul", "Turkey Standard Time"),
    ("Asia/Riyadh", "Arab Standard Time"),
    ("Europe/Minsk", "Belarus Standard Time"),
    ("Europe/Moscow", "Russian Standard Time"),
    ("Africa/Nairobi", "E. Africa Standard Time"),
    ("Asia/Tehran", "Iran Standard Time"),
    ("Asia/Dubai", "Arabian Standard Time"),
    ("Europe/Astrakhan", "Astrakhan Standard Time"),
    ("Asia/Baku", "Azerbaijan Standard Time"),
    ("Europe/Samara", "Russia Time Zone 3"),
    ("Indian/Mauritius", "Mauritius Standard Time"),
    ("Europe/Saratov", "Saratov Standard Time"),
    ("Asia/Tbilisi", "Georgian Standard Time"),
    ("Europe/Volgograd", "Volgograd Standard Time"),
    ("Asia/Yerevan", "Caucasus Standard Time"),
    ("Asia/Kabul", "Afghanistan Standard Time"),
    ("Asia/Tashkent", "West Asia Standard Time"),
    ("Asia/Yekaterinburg", "Ekaterinburg Standard Time"),
    ("Asia/Karachi", "Pakistan Standard Time"),
    ("Asia/Calcutta", "India Standard Time"),
    ("Asia/Kolkata", "India Standard Time"),
    ("Asia/Colombo", "Sri Lanka Standard Time"),
    ("Asia/Katmandu", "Nepal Standard Time"),
    ("Asia/Kathmandu", "Nepal Standard Time"),
    ("Asia/Bishkek", "Central Asia Standard Time"),
    ("Asia/Dhaka", "Bangladesh Standard Time"),
    ("Asia/Omsk", "Omsk Standard Time"),
    ("Asia/Rangoon", "Myanmar Standard Time"),
    ("Asia/Yangon", "Myanmar Standard Time"),
    ("Asia/Bangkok", "SE Asia Standard Time"),
    ("Asia/Barnaul", "Altai Standard Time"),
    ("Asia/Hovd", "W. Mongolia Standard Time"),
    ("Asia/Krasnoyarsk", "North Asia Standard Time"),
    ("Asia/Novosibirsk", "N. Central Asia Standard Time"),
    ("Asia/Tomsk", "Tomsk Standard Time"),
    ("Asia/Shanghai", "China Standard Time"),
    ("Asia/Irkutsk", "North Asia East Standard Time"),
    ("Asia/Singapore", "Singapore Standard Time"),
    ("Australia/Perth", "W. Australia Standard Time"),
    ("Asia/Taipei", "Taipei Standard Time"),
    ("Asia/Ulaanbaatar", "Ulaanbaatar Standard Time"),
    ("Asia/Pyongyang", "North Korea Standard Time"),
    ("Australia/Eucla", "Aus Central W. Standard Time"),
    ("Asia/Chita", "Transbaikal Standard Time"),
    ("Asia/Tokyo", "Tokyo Standard Time"),
    ("Asia/Seoul", "Korea Standard Time"),
    ("Asia/Yakutsk", "Yakutsk Standard Time"),
    ("Australia/Adelaide", "Cen. Australia Standard Time"),
    ("Australia/Darwin", "AUS Central Standard Time"),
    ("Australia/Brisbane", "E. Australia Standard Time"),
    ("Australia/Sydney", "AUS Eastern Standard Time"),
    ("Pacific/Port_Moresby", "West Pacific Standard Time"),
    ("Australia/Hobart", "Tasmania Standard Time"),
    ("Pacific/Auckland", "New Zealand Standard Time"),
];

/// The local zone: the Windows zone of the system's tz database zone (`TZ`, or `/etc/localtime`'s link) when it is
/// known, else by its city among the zones with its standard offset (the names carry standard offsets: Berlin's is
/// +01:00 in summer too), else the first with that offset.
pub(super) fn local() -> (&'static str, &'static str) {
    let zone = system_zone();
    if let Some(known) = zone.as_deref().and_then(by_tzdb) {
        return known;
    }
    let city = zone
        .as_deref()
        .and_then(|zone| zone.rsplit('/').next().map(|city| city.replace('_', " ")));
    by_offset(standard_offset(), city.as_deref())
}

/// The system's tz database zone, as `Area/City`.
fn system_zone() -> Option<String> {
    let zone = std::env::var("TZ")
        .ok()
        .map(|zone| zone.trim_start_matches(':').to_owned())
        .or_else(|| {
            std::fs::read_link("/etc/localtime")
                .ok()
                .map(|path| path.to_string_lossy().into_owned())
        })?;
    let zone = zone.rsplit_once("zoneinfo/").map_or(zone.as_str(), |(_, zone)| zone);
    let zone = zone
        .strip_prefix("posix/")
        .or_else(|| zone.strip_prefix("right/"))
        .unwrap_or(zone);
    Some(zone.to_owned())
}

/// The Windows zone, with its name, of a tz database zone the table knows.
pub(super) fn by_tzdb(zone: &str) -> Option<(&'static str, &'static str)> {
    let (_, windows) = TZDB.iter().find(|(tzdb, _)| *tzdb == zone)?;
    ZONES.iter().find(|(id, _)| id == windows).copied()
}

/// The local zone's standard offset, seconds east of UTC: the smaller of its January and July offsets, as daylight
/// saving adds to it in either hemisphere.
fn standard_offset() -> i32 {
    use chrono::{Datelike as _, TimeZone as _};
    let year = chrono::Local::now().year();
    let offset = |month: u32| {
        chrono::NaiveDate::from_ymd_opt(year, month, 1)
            .and_then(|day| day.and_hms_opt(12, 0, 0))
            .map(|noon| chrono::Local.from_utc_datetime(&noon).offset().local_minus_utc())
    };
    match (offset(1), offset(7)) {
        (Some(january), Some(july)) => january.min(july),
        _ => chrono::Local::now().offset().local_minus_utc(),
    }
}

/// The zone with `seconds` east of UTC whose name holds `city`, else the first with that offset, else UTC.
pub(super) fn by_offset(seconds: i32, city: Option<&str>) -> (&'static str, &'static str) {
    let sign = if seconds < 0 { '-' } else { '+' };
    let minutes = seconds.unsigned_abs() / 60;
    let tag = if minutes == 0 {
        "(UTC)".to_owned()
    } else {
        format!("(UTC{sign}{:02}:{:02})", minutes / 60, minutes % 60)
    };
    let tagged = |name: &str| name.starts_with(&tag) || (minutes == 0 && name.starts_with("(UTC+00:00)"));
    ZONES
        .iter()
        .find(|(_, name)| tagged(name) && city.is_some_and(|city| !city.is_empty() && name.contains(city)))
        .or_else(|| ZONES.iter().find(|(_, name)| tagged(name)))
        .or_else(|| ZONES.iter().find(|(id, _)| *id == "UTC"))
        .copied()
        .unwrap_or(("UTC", "(UTC) Coordinated Universal Time"))
}
