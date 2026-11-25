use std::{
    error::Error,
    fs,
    path::Path,
    thread::sleep,
    time::{Duration, Instant},
};

use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};

use crate::{config::Config, kline::Kline};

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

pub fn fetchklines(
    cfg: &Config,
    interval: &str,
    limit: &str,
) -> Result<Vec<Kline>, Box<dyn Error>> {
    let total: usize = limit.parse()?;
    let url = format!("{}/api/v3/klines", cfg.binance_url);
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
        end_time = batch.first().map(|kline| kline.open_time_ms - 1);

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
    cfg: &Config,
    start_time: i64,
    end_time: i64,
) -> Result<(), Box<dyn Error>> {
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(20))
        .build()?;

    fs::create_dir_all("trades")?;
    let started = Instant::now();
    let total_span = (end_time - start_time) as f64;
    let mut total = 0u64;

    let mut t = start_time - start_time % DAY;
    while t < end_time {
        let w_start = t.max(start_time);
        let w_end = (t + DAY).min(end_time);
        let path = format!("trades/{t}.csv");

        // Уже скачанные дни пропускаем: так можно перезапускать после падения.
        if Path::new(&path).exists() {
            eprintln!("skip {path}");
            t += DAY;
            continue;
        }

        let tmp = format!("{path}.tmp");
        let n = download_window(&client, &cfg.binance_url, w_start, w_end, &tmp)?;
        fs::rename(&tmp, &path)?; // файл без .tmp значит день докачан целиком
        total += n;

        let done = (w_end - start_time) as f64 / total_span;
        let elapsed = started.elapsed().as_secs_f64();
        let eta = if done > 0.0 {
            elapsed / done - elapsed
        } else {
            0.0
        };
        eprintln!(
            "[{:5.1}%] day {t}: {n} trades, total {total}, elapsed {:.0}m, eta {:.0}m",
            done * 100.0,
            elapsed / 60.0,
            eta / 60.0
        );

        t += DAY;
    }
    Ok(())
}
