//! Pure-std feature builder for the OHLCV-only FeaturesV1 schema.
//!
//! Compile this as a sibling module to `rust_intraday_model.rs`, for example:
//!   mod rust_intraday_model;
//!   mod rust_intraday_features;
//!
//! Input bars must be ascending, contiguous one-minute candles. The timestamp
//! is the candle close timestamp in Unix milliseconds. No feature uses a bar
//! after its timestamp. The batch function returns `None` during warm-up or
//! when a required denominator/data value is invalid.

use std::collections::BTreeMap;

use crate::{intraday_model::FeaturesV1, kline::Kline};

pub const MIN_REQUIRED_BARS: usize = 7 * 24 * 60 + 60 + 1;
const MINUTE_MS: i64 = 60_000;
const DAY_MS: i64 = 86_400_000;
const RV_24H_MINUTES: usize = 24 * 60;
const RV_7D_MINUTES: usize = 7 * 24 * 60;

/// Compute FeaturesV1 for each input bar. Entries are None until all required
/// history exists (at least 7 days + 60 minutes + the current bar), or if the
/// row has a zero/invalid denominator. Complexity is approximately O(N log W)
/// due to rolling medians; this is suitable for batch feature generation.
pub fn compute_features_v1_all(bars: &[Kline]) -> Result<Vec<Option<FeaturesV1>>, String> {
    validate_bars(bars)?;
    if bars.len() < MIN_REQUIRED_BARS {
        return Err(format!(
            "need at least {} contiguous 1-minute bars; got {}",
            MIN_REQUIRED_BARS,
            bars.len()
        ));
    }

    let n = bars.len();
    let close: Vec<f64> = bars.iter().map(|b| b.close).collect();
    let high: Vec<f64> = bars.iter().map(|b| b.high).collect();
    let low: Vec<f64> = bars.iter().map(|b| b.low).collect();
    let base_volume: Vec<f64> = bars.iter().map(|b| b.base_volume).collect();
    let quote_volume: Vec<f64> = bars
        .iter()
        .map(|b| b.quote_volume.unwrap_or(b.close * b.base_volume))
        .collect();

    let mut returns = vec![0.0; n];
    let mut squared_return_prefix = vec![0.0; n + 1];
    for i in 1..n {
        returns[i] = (close[i] / close[i - 1]).ln();
        squared_return_prefix[i + 1] = squared_return_prefix[i] + returns[i] * returns[i];
    }
    // Index zero has no prior close; its return/prefix contribution is zero
    // and is never included in a complete feature window.

    let rv60: Vec<Option<f64>> = (0..n)
        .map(|end| rolling_rv(end, 60, &squared_return_prefix))
        .collect();
    let mut rv60_prefix = vec![0.0; n + 1];
    for i in 0..n {
        rv60_prefix[i + 1] = rv60_prefix[i] + rv60[i].unwrap_or(0.0);
    }

    let atr14 = atr_wilder_all(&bars);
    let ema15 = ema_all(&close, 15);
    let ema60 = ema_all(&close, 60);
    let (rolling_high15, rolling_low15) = rolling_extrema(&high, &low, 15);
    let (rolling_high60, rolling_low60) = rolling_extrema(&high, &low, 60);

    let mut quote_prefix = vec![0.0; n + 1];
    for i in 0..n {
        quote_prefix[i + 1] = quote_prefix[i] + quote_volume[i];
    }
    let roll_quote5 = rolling_quote_sums(&quote_prefix, 5);
    let roll_quote60 = rolling_quote_sums(&quote_prefix, 60);
    let median_prior_5 = prior_rolling_medians(&roll_quote5, RV_24H_MINUTES);
    let median_prior_60 = prior_rolling_medians(&roll_quote60, RV_7D_MINUTES);

    let mut output = vec![None; n];
    let warmup_index = MIN_REQUIRED_BARS - 1;
    for t in warmup_index..n {
        let rv15_now = rolling_rv(t, 15, &squared_return_prefix)
            .ok_or_else(|| "RV15 warm-up invariant failed".to_string())?;
        let rv60_now = rv60[t].ok_or_else(|| "RV60 warm-up invariant failed".to_string())?;

        let rv24_start = t - RV_24H_MINUTES;
        let rv7d_start = t - RV_7D_MINUTES;
        let mean_rv60_24h = (rv60_prefix[t] - rv60_prefix[rv24_start]) / RV_24H_MINUTES as f64;
        let mean_rv60_7d = (rv60_prefix[t] - rv60_prefix[rv7d_start]) / RV_7D_MINUTES as f64;

        let atr = atr14[t];
        let vwap15 = rolling_vwap(t, 15, bars, &base_volume);
        let vwap60 = rolling_vwap(t, 60, bars, &base_volume);
        let range_pos15 = range_position(close[t], rolling_low15[t], rolling_high15[t]);
        let range_pos60 = range_position(close[t], rolling_low60[t], rolling_high60[t]);

        let vol_ratio5 = match (roll_quote5[t], median_prior_5[t]) {
            (Some(current), Some(reference)) if reference > 0.0 => current / reference,
            _ => continue,
        };
        let vol_ratio60 = match (roll_quote60[t], median_prior_60[t]) {
            (Some(current), Some(reference)) if reference > 0.0 => current / reference,
            _ => continue,
        };

        if atr <= 0.0 || mean_rv60_24h <= 0.0 || mean_rv60_7d <= 0.0 || !atr.is_finite() {
            continue;
        }
        let Some(vwap15) = vwap15 else { continue };
        let Some(vwap60) = vwap60 else { continue };

        let ret = |minutes: usize| -> f64 { 10_000.0 * (close[t] / close[t - minutes]).ln() };
        let (tod_sin, tod_cos, dow_sin, dow_cos) = time_features_utc(bars[t].close_time_ms);

        let features = FeaturesV1 {
            ret_1m_bps: ret(1),
            ret_3m_bps: ret(3),
            ret_5m_bps: ret(5),
            ret_15m_bps: ret(15),
            ret_30m_bps: ret(30),
            ret_60m_bps: ret(60),
            rv_15m_bps: 10_000.0 * rv15_now,
            rv_60m_bps: 10_000.0 * rv60_now,
            rv60_to_mean_rv60_24h: rv60_now / mean_rv60_24h,
            rv60_to_mean_rv60_7d: rv60_now / mean_rv60_7d,
            atr14_pct_bps: 10_000.0 * atr / close[t],
            vwap_dev_15m_atr: (close[t] - vwap15) / atr,
            vwap_dev_60m_atr: (close[t] - vwap60) / atr,
            ema_dev_15m_atr: (close[t] - ema15[t]) / atr,
            ema_dev_60m_atr: (close[t] - ema60[t]) / atr,
            range_pos_15m: range_pos15,
            range_pos_60m: range_pos60,
            volume_ratio_5m: vol_ratio5,
            volume_ratio_60m: vol_ratio60,
            tod_sin_utc: tod_sin,
            tod_cos_utc: tod_cos,
            dow_sin_utc: dow_sin,
            dow_cos_utc: dow_cos,
        };

        if features.to_vec().iter().all(|x| x.is_finite()) {
            output[t] = Some(features);
        }
    }

    Ok(output)
}

/// Compute only the most recent feature row from a contiguous history slice.
pub fn compute_latest_features_v1(bars: &[Kline]) -> Result<FeaturesV1, String> {
    let all = compute_features_v1_all(bars)?;
    all.last()
        .and_then(|x| *x)
        .ok_or_else(|| "latest feature row is invalid or lacks warm-up".to_string())
}

fn validate_bars(bars: &[Kline]) -> Result<(), String> {
    if bars.is_empty() {
        return Err("bar history is empty".to_string());
    }
    for i in 0..bars.len() {
        let b = bars[i];
        if !b.open.is_finite()
            || !b.high.is_finite()
            || !b.low.is_finite()
            || !b.close.is_finite()
            || !b.base_volume.is_finite()
            || b.quote_volume.map_or(false, |v| !v.is_finite())
        {
            return Err(format!("bar {i} contains a non-finite value"));
        }
        if b.close <= 0.0 || b.low <= 0.0 || b.high < b.low {
            return Err(format!("bar {i} has invalid OHLC prices"));
        }
        if b.base_volume < 0.0 || b.quote_volume.map_or(false, |v| v < 0.0) {
            return Err(format!("bar {i} has negative volume"));
        }
        if i > 0 && b.close_time_ms - bars[i - 1].close_time_ms != MINUTE_MS {
            return Err(format!(
                "bars are not ascending and contiguous at index {i}; expected 60,000 ms spacing"
            ));
        }
    }
    Ok(())
}

fn rolling_rv(end: usize, window: usize, squared_return_prefix: &[f64]) -> Option<f64> {
    if end < window || end + 1 >= squared_return_prefix.len() {
        return None;
    }
    let start_return_index = end - window + 1;
    let sum_sq = squared_return_prefix[end + 1] - squared_return_prefix[start_return_index];
    Some(sum_sq.max(0.0).sqrt())
}

fn atr_wilder_all(bars: &[Kline]) -> Vec<f64> {
    let mut result = vec![0.0; bars.len()];
    if bars.len() <= 14 {
        return result;
    }
    let tr = |i: usize| -> f64 {
        let previous_close = bars[i - 1].close;
        (bars[i].high - bars[i].low)
            .max((bars[i].high - previous_close).abs())
            .max((bars[i].low - previous_close).abs())
    };
    let first_atr = (1..=14).map(tr).sum::<f64>() / 14.0;
    result[14] = first_atr;
    for i in 15..bars.len() {
        result[i] = (13.0 * result[i - 1] + tr(i)) / 14.0;
    }
    result
}

fn ema_all(close: &[f64], period: usize) -> Vec<f64> {
    let mut result = vec![0.0; close.len()];
    if close.len() < period || period == 0 {
        return result;
    }
    let seed = close[..period].iter().sum::<f64>() / period as f64;
    result[period - 1] = seed;
    let alpha = 2.0 / (period as f64 + 1.0);
    for i in period..close.len() {
        result[i] = alpha * close[i] + (1.0 - alpha) * result[i - 1];
    }
    result
}

fn rolling_extrema(high: &[f64], low: &[f64], window: usize) -> (Vec<f64>, Vec<f64>) {
    let mut max_out = vec![0.0; high.len()];
    let mut min_out = vec![0.0; low.len()];
    let mut max_q: std::collections::VecDeque<usize> = std::collections::VecDeque::new();
    let mut min_q: std::collections::VecDeque<usize> = std::collections::VecDeque::new();

    for i in 0..high.len() {
        while let Some(&j) = max_q.back() {
            if high[j] <= high[i] {
                max_q.pop_back();
            } else {
                break;
            }
        }
        max_q.push_back(i);
        while let Some(&j) = min_q.back() {
            if low[j] >= low[i] {
                min_q.pop_back();
            } else {
                break;
            }
        }
        min_q.push_back(i);

        let first_valid = i.saturating_add(1).saturating_sub(window);
        while max_q.front().map_or(false, |&j| j < first_valid) {
            max_q.pop_front();
        }
        while min_q.front().map_or(false, |&j| j < first_valid) {
            min_q.pop_front();
        }
        max_out[i] = high[*max_q.front().unwrap()];
        min_out[i] = low[*min_q.front().unwrap()];
    }
    (max_out, min_out)
}

fn range_position(close: f64, low: f64, high: f64) -> f64 {
    let width = high - low;
    if width <= 0.0 {
        0.5
    } else {
        ((close - low) / width).max(0.0).min(1.0)
    }
}

fn rolling_vwap(end: usize, window: usize, bars: &[Kline], base_volume: &[f64]) -> Option<f64> {
    if end + 1 < window {
        return None;
    }
    let start = end + 1 - window;
    let mut pv = 0.0;
    let mut volume = 0.0;
    for i in start..=end {
        let typical_price = (bars[i].high + bars[i].low + bars[i].close) / 3.0;
        pv += typical_price * base_volume[i];
        volume += base_volume[i];
    }
    if volume <= 0.0 {
        None
    } else {
        Some(pv / volume)
    }
}

fn rolling_quote_sums(prefix: &[f64], window: usize) -> Vec<Option<f64>> {
    let n = prefix.len() - 1;
    let mut result = vec![None; n];
    for end in (window - 1)..n {
        let start = end + 1 - window;
        result[end] = Some(prefix[end + 1] - prefix[start]);
    }
    result
}

fn prior_rolling_medians(values: &[Option<f64>], window: usize) -> Vec<Option<f64>> {
    let mut result = vec![None; values.len()];
    let mut median = RollingMedian::new();
    for t in 0..values.len() {
        if t > 0 {
            if let Some(value) = values[t - 1] {
                median.insert(value);
            }
        }
        if t > window {
            if let Some(value) = values[t - window - 1] {
                median.remove(value);
            }
        }
        if t >= window && median.len() == window {
            result[t] = median.median();
        }
    }
    result
}

/// Rolling median for finite non-negative f64 values. Positive f64 bit
/// patterns sort in numeric order, so they can be keys in BTreeMap.
#[derive(Default)]
struct RollingMedian {
    lower: BTreeMap<u64, usize>,
    upper: BTreeMap<u64, usize>,
    lower_len: usize,
    upper_len: usize,
}

impl RollingMedian {
    fn new() -> Self {
        Self::default()
    }
    fn len(&self) -> usize {
        self.lower_len + self.upper_len
    }

    fn key(value: f64) -> u64 {
        if value == 0.0 {
            0.0_f64.to_bits()
        } else {
            value.to_bits()
        }
    }

    fn insert(&mut self, value: f64) {
        let key = Self::key(value);
        let goes_lower = self
            .lower
            .keys()
            .next_back()
            .map_or(true, |&largest| key <= largest);
        if goes_lower {
            map_insert(&mut self.lower, key);
            self.lower_len += 1;
        } else {
            map_insert(&mut self.upper, key);
            self.upper_len += 1;
        }
        self.rebalance();
    }

    fn remove(&mut self, value: f64) {
        let key = Self::key(value);
        if self.lower.contains_key(&key) {
            map_remove(&mut self.lower, key);
            self.lower_len -= 1;
        } else if self.upper.contains_key(&key) {
            map_remove(&mut self.upper, key);
            self.upper_len -= 1;
        }
        self.rebalance();
    }

    fn rebalance(&mut self) {
        while self.lower_len > self.upper_len + 1 {
            let key = *self.lower.keys().next_back().unwrap();
            map_remove(&mut self.lower, key);
            map_insert(&mut self.upper, key);
            self.lower_len -= 1;
            self.upper_len += 1;
        }
        while self.lower_len < self.upper_len {
            let key = *self.upper.keys().next().unwrap();
            map_remove(&mut self.upper, key);
            map_insert(&mut self.lower, key);
            self.upper_len -= 1;
            self.lower_len += 1;
        }
    }

    fn median(&self) -> Option<f64> {
        if self.lower_len == 0 {
            return None;
        }
        let lower = f64::from_bits(*self.lower.keys().next_back()?);
        if self.lower_len == self.upper_len {
            let upper = f64::from_bits(*self.upper.keys().next()?);
            Some((lower + upper) * 0.5)
        } else {
            Some(lower)
        }
    }
}

fn map_insert(map: &mut BTreeMap<u64, usize>, key: u64) {
    *map.entry(key).or_insert(0) += 1;
}

fn map_remove(map: &mut BTreeMap<u64, usize>, key: u64) {
    if let Some(count) = map.get_mut(&key) {
        *count -= 1;
        if *count == 0 {
            map.remove(&key);
        }
    }
}

fn time_features_utc(timestamp_ms: i64) -> (f64, f64, f64, f64) {
    let minute_of_day = timestamp_ms.div_euclid(MINUTE_MS).rem_euclid(1440) as f64;
    // 1970-01-01 was Thursday; Monday=0 therefore Unix day 0 maps to 3.
    let day_since_epoch = timestamp_ms.div_euclid(DAY_MS);
    let weekday_monday_zero = (day_since_epoch + 3).rem_euclid(7) as f64;
    let day_angle = 2.0 * std::f64::consts::PI * minute_of_day / 1440.0;
    let week_angle = 2.0 * std::f64::consts::PI * weekday_monday_zero / 7.0;
    (
        day_angle.sin(),
        day_angle.cos(),
        week_angle.sin(),
        week_angle.cos(),
    )
}
