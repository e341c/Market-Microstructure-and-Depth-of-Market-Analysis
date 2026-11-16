use std::error::Error;

use reqwest::blocking::Client;
use serde::Deserialize;

use crate::config::Config;

pub fn fetchdepth(cfg: &Config) -> Result<String, reqwest::Error> {
    let url = format!("{}/api/v3/depth?symbol=BTCUSDT&limit=5", cfg.binance_url);

    let raw_text = reqwest::blocking::get(url)?.text()?;

    println!("Первые 100 символов ответа:\n{}", &raw_text[..]);
    Ok(raw_text)
}

#[derive(Debug, Deserialize)]
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

pub fn fetchklines(cfg: &Config, interval: &str, limit: &str) -> Result<Vec<Kline>, Box<dyn Error>> {
    let url = format!("{}/api/v3/klines", cfg.binance_url);

    let client = Client::new();
    let response = client
        .get(url)
        .query(&[
            ("symbol", "BTCUSDT"),
            ("interval", interval),
            ("limit", limit),
        ])
        .send()? // без .await
        .error_for_status()?;

    let klines: Vec<Kline> = response.json()?; // без .await

    Ok(klines)
}
