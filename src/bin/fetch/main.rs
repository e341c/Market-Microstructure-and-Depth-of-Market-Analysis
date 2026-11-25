use std::{
    error::Error,
    fs,
    path::Path,
    thread::sleep,
    time::{Duration, Instant},
};

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    mpsc,
};
use std::thread;

use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
pub struct AggTrade {
    #[serde(rename = "a")]
    pub id: u64,
    #[serde(rename = "p")]
    pub price: String,
    #[serde(rename = "q")]
    pub quantity: String,
    #[serde(rename = "f")]
    pub first_trade_id: u64,
    #[serde(rename = "l")]
    pub last_trade_id: u64,
    #[serde(rename = "T")]
    pub time: i64,
    #[serde(rename = "m")]
    pub buyer_is_maker: bool,
    #[serde(rename = "M")]
    pub best_match: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[allow(dead_code)]
pub struct Kline {
    pub open_time: i64,
    pub open: String,
    pub high: String,
    pub low: String,
    pub close: String,
    pub volume: String,
    pub close_time: i64,
    pub quote_asset_volume: String,
    pub number_of_trades: u64,
    pub taker_buy_base_asset_volume: String,
    pub taker_buy_quote_asset_volume: String,
    pub ignore: String,
}

// #[derive(Debug, Deserialize, Serialize)]
// pub struct Kline {
//     pub open_time_ms: i64,
//     /// Unix timestamp (milliseconds) at the close of this 1-minute candle.
//     pub close_time_ms: i64,
//     pub open: String,
//     pub high: String,
//     pub low: String,
//     pub close: String,
//     /// Base-asset volume from the candle.
//     pub base_volume: String,
//     /// Quote notional volume, if the source provides it.
//     pub quote_volume: Option<f64>,
// }
// #[derive(Clone, Copy, Debug, Deserialize, Serialize)]
// pub struct Kline {
//     pub open_time_ms: i64,
//     /// Unix timestamp (milliseconds) at the close of this 1-minute candle.
//     pub close_time_ms: i64,
//     pub open: f64,
//     pub high: f64,
//     pub low: f64,
//     pub close: f64,
//     /// Base-asset volume from the candle.
//     pub base_volume: f64,
//     /// Quote notional volume, if the source provides it.
//     pub quote_volume: Option<f64>,
// }

pub fn fetchklines(url: &str, interval: &str, limit: &str) -> Result<Vec<Kline>, Box<dyn Error>> {
    let total: usize = limit.parse()?;
    let url = format!("{}/api/v3/klines", url);
    let client = Client::new();

    let mut klines = Vec::with_capacity(total);
    let mut end_time: Option<i64> = None;

    while klines.len() < total {
        let batch_limit = (total - klines.len()).min(1000);

        let mut request = client
            .get(&url)
            .query(&[("symbol", "BTCUSDT"), ("interval", interval)])
            .query(&[("limit", batch_limit.to_string())]);

        if let Some(end) = end_time {
            request = request.query(&[("endTime", end.to_string())]);
        }

        let mut batch: Vec<Kline> = request.send()?.error_for_status()?.json()?;

        if batch.is_empty() {
            break;
        }

        // Следующая страница должна закончиться перед самой ранней свечой
        // из текущей страницы.
        end_time = batch.first().map(|kline| kline.open_time - 1);

        // Binance возвращает свечи от старых к новым. Добавляем страницу
        // перед уже полученными, чтобы сохранить этот порядок.
        batch.append(&mut klines);
        klines = batch;
    }

    if klines.len() > total {
        klines.drain(..klines.len() - total);
    }

    let mut writer = csv::Writer::from_path("klines.csv")?;

    writer.write_record([
        "open_time",
        "open",
        "high",
        "low",
        "close",
        "volume",
        "close_time",
        "quote_asset_volume",
        "number_of_trades",
        "taker_buy_base_asset_volume",
        "taker_buy_quote_asset_volume",
        "ignore",
    ])?;

    for kline in &klines {
        writer.serialize(kline)?;
    }

    writer.flush()?;

    Ok(klines)
}

const DAY: i64 = 86_400_000;

fn get_batch(
    client: &Client,
    url: &str,
    params: &[(&str, String)],
) -> Result<Vec<AggTrade>, Box<dyn Error>> {
    let mut attempt = 0;
    loop {
        let res = client.get(url).query(params).send();
        match res {
            Ok(resp) => {
                let st = resp.status();
                if st.as_u16() == 429 || st.as_u16() == 418 {
                    let wait = resp
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(30);
                    eprintln!("rate limit {st}, sleep {wait}s");
                    sleep(Duration::from_secs(wait));
                    continue;
                }
                return Ok(resp.error_for_status()?.json()?);
            }
            Err(e) if attempt < 5 => {
                attempt += 1;
                eprintln!("request error: {e}, retry {attempt}/5");
                sleep(Duration::from_secs(2 * attempt));
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// Пишет сделки start <= time < end в файл path. Возвращает количество.
fn download_window(
    client: &Client,
    base: &str,
    start: i64,
    end: i64,
    path: &str,
) -> Result<u64, Box<dyn Error>> {
    let url = format!("{base}/api/v3/aggTrades");
    let mut writer = csv::Writer::from_path(path)?;
    let mut from_id: Option<u64> = None;
    let mut count = 0u64;
    let mut pages = 0u64;
    let t0 = Instant::now();

    'outer: loop {
        let mut params = vec![
            ("symbol", "BTCUSDT".to_string()),
            ("limit", "1000".to_string()),
        ];
        match from_id {
            Some(id) => params.push(("fromId", id.to_string())),
            // Только startTime: окно в сутки с endTime Binance не разрешит.
            None => params.push(("startTime", start.to_string())),
        }

        let batch = get_batch(client, &url, &params)?;
        let full = batch.len() == 1000;
        if batch.is_empty() {
            break;
        }

        for t in batch {
            if t.time >= end {
                break 'outer;
            }
            from_id = Some(t.id + 1);
            writer.serialize(&t)?;
            count += 1;
        }

        pages += 1;
        if pages % 50 == 0 {
            writer.flush()?; // данные реально на диске
            let rate = count as f64 / t0.elapsed().as_secs_f64();
            eprintln!("  page {pages}, trades {count}, {rate:.0} trades/s");
        }
        if !full {
            break;
        }
    }

    writer.flush()?;
    Ok(count)
}

pub fn fetch_agg_trades_range(
    url: &str,
    start_time: i64,
    end_time: i64,
) -> Result<(), Box<dyn Error>> {
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(20))
        .build()?;

    fs::create_dir_all("trades")?;

    let mut windows = Vec::new();
    let mut t = start_time - start_time % DAY;

    while t < end_time {
        let w_start = t.max(start_time);
        let w_end = (t + DAY).min(end_time);
        let path = format!("trades/{t}.csv");

        if Path::new(&path).exists() {
            eprintln!("skip {path}");
        } else {
            windows.push((t, w_start, w_end, path));
        }

        t += DAY;
    }

    if windows.is_empty() {
        return Ok(());
    }

    let worker_count = 4.min(windows.len());
    let next = AtomicUsize::new(0);
    let (tx, rx) = mpsc::channel();

    let mut errors = Vec::new();
    let mut total = 0u64;
    let mut completed = 0usize;
    let started = Instant::now();

    thread::scope(|scope| {
        for _ in 0..worker_count {
            let tx = tx.clone();
            let client = client.clone();
            let windows = &windows;
            let next = &next;

            scope.spawn(move || {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some((t, w_start, w_end, path)) = windows.get(i) else {
                        break;
                    };

                    let tmp = format!("{path}.tmp");
                    let result = download_window(&client, url, *w_start, *w_end, &tmp)
                        .and_then(|n| {
                            fs::rename(&tmp, path)?;
                            Ok(n)
                        })
                        .map_err(|e| {
                            let _ = fs::remove_file(&tmp);
                            e.to_string()
                        });

                    if tx.send((*t, path.clone(), result)).is_err() {
                        break;
                    }
                }
            });
        }

        drop(tx);

        for (t, path, result) in rx {
            completed += 1;

            match result {
                Ok(n) => {
                    total += n;
                    eprintln!(
                        "[{completed}/{}] day {t}: {n} trades, total {total}",
                        windows.len()
                    );
                }
                Err(e) => {
                    eprintln!("failed {path}: {e}");
                    errors.push(format!("{path}: {e}"));
                }
            }
        }
    });

    if !errors.is_empty() {
        return Err(std::io::Error::new(std::io::ErrorKind::Other, errors.join("\n")).into());
    }

    eprintln!(
        "download complete: {total} trades in {:.1} minutes",
        started.elapsed().as_secs_f64() / 60.0
    );

    Ok(())
}

pub fn fetchklines_range(
    url: &str,
    interval: &str,
    start_time_ms: i64,
    end_time_ms: i64,
) -> Result<Vec<Kline>, Box<dyn Error>> {
    if start_time_ms > end_time_ms {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "start_time_ms must be <= end_time_ms",
        )
        .into());
    }

    const BATCH_LIMIT: usize = 1000;

    let url = format!("{}/api/v3/klines", url);
    let client = Client::new();
    let mut klines: Vec<Kline> = Vec::new();
    let mut cursor = start_time_ms;

    loop {
        let params = [
            ("startTime", cursor.to_string()),
            ("endTime", end_time_ms.to_string()),
            ("limit", BATCH_LIMIT.to_string()),
        ];

        let batch: Vec<Kline> = client
            .get(&url)
            .query(&[("symbol", "BTCUSDT"), ("interval", interval)])
            .query(&params)
            .send()?
            .error_for_status()?
            .json()?;

        if batch.is_empty() {
            break;
        }

        let batch_len = batch.len();
        let last_open_time = batch.last().unwrap().open_time;

        for kline in batch {
            if kline.open_time >= start_time_ms
                && kline.open_time <= end_time_ms
                && klines
                    .last()
                    .map_or(true, |last| last.open_time < kline.open_time)
            {
                klines.push(kline);
            }
        }

        if last_open_time >= end_time_ms || batch_len < BATCH_LIMIT {
            break;
        }

        let next_cursor = last_open_time.checked_add(1).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "timestamp overflow")
        })?;

        if next_cursor <= cursor {
            break;
        }

        cursor = next_cursor;
    }

    let mut writer = csv::Writer::from_path("klines.csv")?;

    writer.write_record([
        "open_time",
        "open",
        "high",
        "low",
        "close",
        "volume",
        "close_time",
        "quote_asset_volume",
        "number_of_trades",
        "taker_buy_base_asset_volume",
        "taker_buy_quote_asset_volume",
        "ignore",
    ])?;

    for kline in &klines {
        writer.serialize(kline)?;
    }

    writer.flush()?;

    Ok(klines)
}

// files=74, trades=93152464, skipped_lines=74, minutes=105970, gaps=0
// диапазон потока: [1765101000000, 1771459140000] мс; saved: flow_1m.csv
fn main() {
    let url = "https://api.binance.com";
    let interval = "1m";
    let limit = "200000";

    // let klines = fetchklines_range(url, interval, 1765101000000, 1771459140000)
    //     .expect("cannot fetch klines range");

    let test_start_time: i64 = 1789461300000;
    let test_end_time: i64 = 1795461240000;

    fetch_agg_trades_range(url, test_start_time, test_end_time);
}
