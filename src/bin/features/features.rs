//! Feature engineering for an SMA crossover trading strategy.
//!
//! All rows stay aligned with the input candles. Indicator values are `None`
//! during their warm-up period or when a calculation would divide by zero.

use std::error::Error;
use std::fmt;

use market_microstructure::kline::Kline;

/// Periods used by the feature builder. SMA and EMA use the same fast/slow
/// periods; the remaining periods can be set independently.
#[derive(Clone, Copy, Debug)]
pub struct FeatureConfig {
    pub sma1_period: usize,
    pub sma2_period: usize,
    pub rsi_period: usize,
    pub stochastic_k_period: usize,
    pub stochastic_d_period: usize,
    pub roc_period: usize,
    pub momentum_period: usize,
    pub volume_roc_period: usize,
}

#[derive(Clone, Debug)]
pub struct FeatureRow {
    pub index: usize,

    // Original market columns.
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
    pub weighted_price: Option<f64>,

    // Moving averages and normalized distances, in percent.
    pub sma1: Option<f64>,
    pub sma2: Option<f64>,
    /// Absolute SMA1 - SMA2 difference in the quote-price units (e.g. USDT).
    pub sma_difference: Option<f64>,
    pub close_vs_sma1_pct: Option<f64>,
    pub close_vs_sma2_pct: Option<f64>,
    pub sma_spread_pct: Option<f64>,
    pub ema1: Option<f64>,
    pub ema2: Option<f64>,
    pub ema_spread_pct: Option<f64>,

    // Other technical indicators.
    pub rsi: Option<f64>,
    pub stochastic_k: Option<f64>,
    pub stochastic_d: Option<f64>,
    pub roc_pct: Option<f64>,
    pub momentum: Option<f64>,
    pub volume_roc_pct: Option<f64>,
    pub volume_momentum: Option<f64>,
    // Signal
    // pub signal: Option<f64>,
}

/// The strategy target is returned separately so it is not accidentally
/// included among the model input features.
#[derive(Clone, Debug)]
pub struct FeatureSet {
    pub rows: Vec<FeatureRow>,
    /// Same-candle strategy label: 1 when SMA1 > SMA2, 0 when SMA1 < SMA2.
    /// It is `None` during SMA warm-up and when the two averages are equal.
    pub signal: Vec<Option<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FeatureError {
    InvalidPeriod { name: &'static str, value: usize },
    FastSmaMustBeShorter,
    NonFiniteValue { row: usize, field: &'static str },
}

impl fmt::Display for FeatureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPeriod { name, value } => {
                write!(f, "period `{name}` must be greater than zero (got {value})")
            }
            Self::FastSmaMustBeShorter => {
                write!(f, "sma1_period must be smaller than sma2_period")
            }
            Self::NonFiniteValue { row, field } => {
                write!(f, "non-finite value in `{field}` at candle index {row}")
            }
        }
    }
}

impl Error for FeatureError {}

/// Calculate SMA, EMA, RSI, stochastic %K/%D, ROC, and momentum features.
///
/// The caller must provide candles in chronological order. The target `signal`
/// is defined on the same candle as its SMA values; for a forecasting model,
/// use the next candle's signal as the target for the current row.
pub fn create_features(
    candles: &[Kline],
    config: FeatureConfig,
) -> Result<FeatureSet, FeatureError> {
    validate_config(config)?;
    validate_candles(candles)?;

    let close: Vec<f64> = candles.iter().map(|c| c.close).collect();
    let volume: Vec<f64> = candles.iter().map(|c| c.base_volume).collect();

    let sma1 = rolling_sma(&close, config.sma1_period);
    let sma2 = rolling_sma(&close, config.sma2_period);
    let ema1 = exponential_moving_average(&close, config.sma1_period);
    let ema2 = exponential_moving_average(&close, config.sma2_period);
    let rsi = relative_strength_index(&close, config.rsi_period);
    let stochastic_k = stochastic_k(candles, config.stochastic_k_period);
    let stochastic_d = rolling_optional_sma(&stochastic_k, config.stochastic_d_period);
    let roc = rate_of_change(&close, config.roc_period);
    let momentum_vec = momentum(&close, config.momentum_period);
    let volume_roc = rate_of_change(&volume, config.volume_roc_period);
    let volume_momentum = momentum(&volume, config.volume_roc_period);

    let mut rows = Vec::with_capacity(candles.len());
    let mut signal = Vec::with_capacity(candles.len());

    for (i, candle) in candles.iter().enumerate() {
        rows.push(FeatureRow {
            index: i,
            open: candle.open,
            high: candle.high,
            low: candle.low,
            close: candle.close,
            volume: candle.base_volume,
            weighted_price: candle.weighted_price,
            sma1: sma1[i],
            sma2: sma2[i],
            sma_difference: pair_difference(sma1[i], sma2[i]),
            close_vs_sma1_pct: percent_distance(candle.close, sma1[i]),
            close_vs_sma2_pct: percent_distance(candle.close, sma2[i]),
            sma_spread_pct: pair_spread(sma1[i], sma2[i]),
            ema1: ema1[i],
            ema2: ema2[i],
            ema_spread_pct: pair_spread(ema1[i], ema2[i]),
            rsi: rsi[i],
            stochastic_k: stochastic_k[i],
            stochastic_d: stochastic_d[i],
            roc_pct: roc[i],
            momentum: momentum_vec[i],
            volume_roc_pct: volume_roc[i],
            volume_momentum: volume_momentum[i],
        });

        signal.push(match (sma1[i], sma2[i]) {
            (Some(fast), Some(slow)) if fast > slow => Some(1),
            (Some(fast), Some(slow)) if fast < slow => Some(0),
            _ => None,
        });
    }

    Ok(FeatureSet { rows, signal })
}

fn validate_config(config: FeatureConfig) -> Result<(), FeatureError> {
    let periods = [
        ("sma1_period", config.sma1_period),
        ("sma2_period", config.sma2_period),
        ("rsi_period", config.rsi_period),
        ("stochastic_k_period", config.stochastic_k_period),
        ("stochastic_d_period", config.stochastic_d_period),
        ("roc_period", config.roc_period),
        ("momentum_period", config.momentum_period),
        ("volume_roc_period", config.volume_roc_period),
    ];
    for (name, value) in periods {
        if value == 0 {
            return Err(FeatureError::InvalidPeriod { name, value });
        }
    }
    if config.sma1_period >= config.sma2_period {
        return Err(FeatureError::FastSmaMustBeShorter);
    }
    Ok(())
}

fn validate_candles(candles: &[Kline]) -> Result<(), FeatureError> {
    for (row, candle) in candles.iter().enumerate() {
        for (field, value) in [
            ("open", candle.open),
            ("high", candle.high),
            ("low", candle.low),
            ("close", candle.close),
            ("volume", candle.base_volume),
        ] {
            if !value.is_finite() {
                return Err(FeatureError::NonFiniteValue { row, field });
            }
        }
        if candle
            .weighted_price
            .is_some_and(|value| !value.is_finite())
        {
            return Err(FeatureError::NonFiniteValue {
                row,
                field: "weighted_price",
            });
        }
    }
    Ok(())
}

fn rolling_sma(values: &[f64], period: usize) -> Vec<Option<f64>> {
    let mut result = vec![None; values.len()];
    let mut sum = 0.0;
    for i in 0..values.len() {
        sum += values[i];
        if i >= period {
            sum -= values[i - period];
        }
        if i + 1 >= period {
            result[i] = Some(sum / period as f64);
        }
    }
    result
}

fn rolling_optional_sma(values: &[Option<f64>], period: usize) -> Vec<Option<f64>> {
    let mut result = vec![None; values.len()];
    if values.len() < period {
        return result;
    }
    for end in (period - 1)..values.len() {
        let window = &values[(end + 1 - period)..=end];
        if window.iter().all(Option::is_some) {
            let sum: f64 = window.iter().map(|value| value.unwrap()).sum();
            result[end] = Some(sum / period as f64);
        }
    }
    result
}

fn exponential_moving_average(values: &[f64], period: usize) -> Vec<Option<f64>> {
    let mut result = vec![None; values.len()];
    if values.len() < period {
        return result;
    }

    let seed = values[..period].iter().sum::<f64>() / period as f64;
    result[period - 1] = Some(seed);
    let alpha = 2.0 / (period as f64 + 1.0);
    let mut previous = seed;

    for i in period..values.len() {
        previous = alpha * values[i] + (1.0 - alpha) * previous;
        result[i] = Some(previous);
    }
    result
}

fn relative_strength_index(close: &[f64], period: usize) -> Vec<Option<f64>> {
    let mut result = vec![None; close.len()];
    if close.len() <= period {
        return result;
    }

    let mut average_gain = 0.0;
    let mut average_loss = 0.0;
    for i in 1..=period {
        let change = close[i] - close[i - 1];
        if change > 0.0 {
            average_gain += change;
        } else {
            average_loss -= change;
        }
    }
    average_gain /= period as f64;
    average_loss /= period as f64;
    result[period] = Some(rsi_value(average_gain, average_loss));

    for i in (period + 1)..close.len() {
        let change = close[i] - close[i - 1];
        let gain = change.max(0.0);
        let loss = (-change).max(0.0);
        average_gain = (average_gain * (period as f64 - 1.0) + gain) / period as f64;
        average_loss = (average_loss * (period as f64 - 1.0) + loss) / period as f64;
        result[i] = Some(rsi_value(average_gain, average_loss));
    }
    result
}

fn rsi_value(average_gain: f64, average_loss: f64) -> f64 {
    match (average_gain, average_loss) {
        (0.0, 0.0) => 50.0,
        (_, 0.0) => 100.0,
        (0.0, _) => 0.0,
        (gain, loss) => 100.0 - 100.0 / (1.0 + gain / loss),
    }
}

fn stochastic_k(candles: &[Kline], period: usize) -> Vec<Option<f64>> {
    let mut result = vec![None; candles.len()];
    if candles.len() < period {
        return result;
    }

    for end in (period - 1)..candles.len() {
        let window = &candles[(end + 1 - period)..=end];
        let lowest_low = window
            .iter()
            .map(|candle| candle.low)
            .fold(f64::INFINITY, f64::min);
        let highest_high = window
            .iter()
            .map(|candle| candle.high)
            .fold(f64::NEG_INFINITY, f64::max);
        let range = highest_high - lowest_low;
        if range != 0.0 {
            result[end] = Some((candles[end].close - lowest_low) / range * 100.0);
        }
    }
    result
}

fn rate_of_change(values: &[f64], period: usize) -> Vec<Option<f64>> {
    let mut result = vec![None; values.len()];
    for i in period..values.len() {
        let previous = values[i - period];
        if previous != 0.0 {
            result[i] = Some((values[i] / previous - 1.0) * 100.0);
        }
    }
    result
}

fn momentum(values: &[f64], period: usize) -> Vec<Option<f64>> {
    let mut result = vec![None; values.len()];
    for i in period..values.len() {
        result[i] = Some(values[i] - values[i - period]);
    }
    result
}

fn percent_distance(value: f64, baseline: Option<f64>) -> Option<f64> {
    baseline
        .filter(|base| *base != 0.0)
        .map(|base| (value / base - 1.0) * 100.0)
}

fn pair_spread(fast: Option<f64>, slow: Option<f64>) -> Option<f64> {
    match (fast, slow) {
        (Some(fast), Some(slow)) if slow != 0.0 => Some((fast / slow - 1.0) * 100.0),
        _ => None,
    }
}

fn pair_difference(fast: Option<f64>, slow: Option<f64>) -> Option<f64> {
    match (fast, slow) {
        (Some(fast), Some(slow)) => Some(fast - slow),
        _ => None,
    }
}
