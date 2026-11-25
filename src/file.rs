use std::fs::File;
use std::io::{BufRead, BufReader};

use crate::kline::Kline;

pub fn load_minute_bars_csv(path: &str) -> Result<Vec<Kline>, String> {
    let file = File::open(path).map_err(|e| format!("cannot open {path}: {e}"))?;
    let reader = BufReader::new(file);
    let mut lines = reader.lines();
    lines
        .next()
        .ok_or_else(|| "CSV has no header".to_string())?
        .map_err(|e| format!("cannot read CSV header: {e}"))?;

    let mut bars = Vec::new();
    for (line_idx, line) in lines.enumerate() {
        let line = line.map_err(|e| format!("CSV read error at data row {}: {e}", line_idx + 2))?;
        if line.trim().is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.trim().split(',').collect();
        if cols.len() != 12 {
            return Err(format!(
                "row {} must have 12 Binance kline columns, got {}",
                line_idx + 2,
                cols.len()
            ));
        }
        let parse = |i: usize, name: &str| -> Result<f64, String> {
            cols[i]
                .parse::<f64>()
                .map_err(|_| format!("invalid {name} at CSV row {}", line_idx + 2))
        };
        let open_time_ms = cols[0]
            .parse::<i64>()
            .map_err(|_| format!("invalid open_time at CSV row {}", line_idx + 2))?;
        let close_time_ms = cols[6]
            .parse::<i64>()
            .map_err(|_| format!("invalid close_time at CSV row {}", line_idx + 2))?;
        let quote_volume = if !cols[7].trim().is_empty() {
            Some(parse(7, "quote_asset_volume")?)
        } else {
            None
        };

        let mut kline = Kline {
            open_time_ms,
            close_time_ms,
            open: parse(1, "open")?,
            high: parse(2, "high")?,
            low: parse(3, "low")?,
            close: parse(4, "close")?,
            base_volume: parse(5, "base_volume")?,
            quote_volume,
            weighted_price: None,
        };

        kline.weighted_price = kline.vwap();
        bars.push(kline);
    }
    Ok(bars)
}
