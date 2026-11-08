//! FIX UTCTimestamp without chrono.

use std::time::{SystemTime, UNIX_EPOCH};

/// `YYYYMMDD-HH:MM:SS.ffffff` in UTC.
pub fn utc_timestamp() -> String {
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    let secs = d.as_secs() as i64;
    let micros = d.subsec_micros();
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);

    // civil-from-days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };

    format!(
        "{:04}{:02}{:02}-{:02}:{:02}:{:02}.{:06}",
        y,
        m,
        day,
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        micros
    )
}
