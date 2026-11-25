use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub struct Kline {
    pub open_time_ms: i64,
    /// Unix timestamp (milliseconds) at the close of this 1-minute candle.
    pub close_time_ms: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    /// Base-asset volume from the candle.
    pub base_volume: f64,
    /// Quote notional volume, if the source provides it.
    pub quote_volume: Option<f64>,
    pub weighted_price: Option<f64>,
}

impl Kline {
    pub fn vwap(&self) -> Option<f64> {
        let quote = self.quote_volume?;

        if !self.base_volume.is_finite() || self.base_volume <= 0.0 || !quote.is_finite() {
            return None;
        }

        let price = quote / self.base_volume;
        price.is_finite().then_some(price)
    }
}
