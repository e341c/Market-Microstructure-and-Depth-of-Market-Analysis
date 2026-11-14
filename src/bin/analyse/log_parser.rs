use std::{fs, io};

pub struct Log {
    /// Время строки в микросекундах от Unix epoch (UTC).
    pub ts_us: Vec<i64>,
    /// Колонки, начиная с "best ask price" (ask, bid, microprice, obi, obi1s, ofi, ofi500).
    pub data: Vec<Vec<f64>>,
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// "20261108-12:26:53.329506" -> микросекунды от epoch.
fn parse_ts(s: &str) -> Option<i64> {
    let (date, time) = s.trim().split_once('-')?;
    if date.len() != 8 {
        return None;
    }
    let y: i64 = date.get(0..4)?.parse().ok()?;
    let mo: i64 = date.get(4..6)?.parse().ok()?;
    let d: i64 = date.get(6..8)?.parse().ok()?;

    let mut it = time.split(':');
    let h: i64 = it.next()?.parse().ok()?;
    let mi: i64 = it.next()?.parse().ok()?;
    let sec_part = it.next()?;

    let (sec, frac) = sec_part.split_once('.').unwrap_or((sec_part, "0"));
    let sec: i64 = sec.parse().ok()?;
    let mut frac_s: String = frac.chars().take(6).collect();
    while frac_s.len() < 6 {
        frac_s.push('0');
    }
    let us: i64 = frac_s.parse().ok()?;

    let days = days_from_civil(y, mo, d);
    Some(((days * 24 + h) * 60 + mi) * 60_000_000 + sec * 1_000_000 + us)
}

pub fn read_log_file(n: &str) -> io::Result<Log> {
    let contents = fs::read_to_string(n)?;

    let mut ts_us = Vec::new();
    let mut data = Vec::new();

    for line in contents.lines() {
        let line = line.trim().trim_end_matches(';');
        let r: Vec<&str> = line.split('|').collect();
        if r.len() != 11 {
            continue;
        }

        let Some(ts) = parse_ts(r[1]) else {
            continue; // заголовок и битые строки
        };

        let parsed: Option<Vec<f64>> = r[4..]
            .iter()
            .map(|v| v.trim().parse::<f64>().ok())
            .collect();

        let Some(row) = parsed else {
            continue; // битое число: пропускаем строку целиком
        };

        ts_us.push(ts);
        data.push(row);
    }

    Ok(Log { ts_us, data })
}
