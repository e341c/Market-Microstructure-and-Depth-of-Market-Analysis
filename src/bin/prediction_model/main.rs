//! End-to-end initialization example using only the Rust standard library.
//!
//! Files expected beside this file:
//!   intraday_model.rs
//!   intraday_features.rs
//!
//! Binance kline CSV columns (header required):
//! open_time,open,high,low,close,volume,close_time,quote_asset_volume,
//! number_of_trades,taker_buy_base_asset_volume,taker_buy_quote_asset_volume,ignore
//!
//! Set COST_RT_BPS to a conservative round-trip cost estimate for the
//! configured reference notional. This OHLCV-only example is a research
//! scaffold, not a production fill simulator.

use market_microstructure::intraday_features;
use market_microstructure::intraday_model;

use std::env;
use std::fs::File;
use std::io::{BufRead, BufReader};

use intraday_features::{MIN_REQUIRED_BARS, compute_features_v1_all, compute_latest_features_v1};
use intraday_model::{
    IntradayModel, ModelConfig, Outcome, Policy, Side, TrainingSample, choose_signal_v1,
    classify_outcome,
};
use market_microstructure::fetchdata::Kline;

const HORIZON_BARS: usize = 60;
const EMBARGO_BARS: usize = 60;

#[derive(Clone, Copy)]
struct CostAndBarrierConfig {
    round_trip_cost_bps: f64,
    edge_buffer_bps: f64,
    stop_buffer_bps: f64,
    take_profit_atr: f64,
    stop_loss_atr: f64,
}

struct LabeledRow {
    event_idx: usize,
    label_end_idx: usize,
    sample: TrainingSample,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let csv_path = env::args()
        .nth(1)
        .ok_or_else(|| "usage: rust_train_model_example <ohlcv.csv>".to_string())?;
    let cost_rt_bps: f64 = env::var("COST_RT_BPS")
        .map_err(|_| "set COST_RT_BPS to fees + spread + slippage + other costs".to_string())?
        .parse()
        .map_err(|_| "COST_RT_BPS must be a number".to_string())?;

    let bars = load_minute_bars_csv(&csv_path)?;
    let first_event = MIN_REQUIRED_BARS - 1;
    let last_decision = bars.len().saturating_sub(HORIZON_BARS + 1);
    if last_decision < first_event {
        return Err(format!(
            "not enough data after warm-up: need more than {} bars; got {}",
            first_event + HORIZON_BARS,
            bars.len()
        ));
    }
    let usable_events = last_decision - first_event + 1;
    let split_gap = HORIZON_BARS + EMBARGO_BARS;
    if usable_events <= 2 * split_gap + 100 {
        return Err(format!(
            "not enough usable decision times after warm-up: got {usable_events}; need more than {}",
            2 * split_gap + 100
        ));
    }

    // 1. Build all 23 OHLCV features causally. Early rows are None until warm-up.
    let features = compute_features_v1_all(&bars)?;

    // These are initial research settings. Tune only inside chronological
    // train/validation windows, then freeze them before final OOS evaluation.
    let cost_cfg = CostAndBarrierConfig {
        round_trip_cost_bps: cost_rt_bps,
        edge_buffer_bps: 2.0,
        stop_buffer_bps: 2.0,
        take_profit_atr: 1.5,
        stop_loss_atr: 1.0,
    };

    // 2. Make two historical candidate trades per decision time (long + short).
    // Labels use only future bars; model inputs use features available at t.
    let rows = build_labeled_rows(&bars, &features, cost_cfg)?;
    if rows.len() < 100 {
        return Err("too few labeled rows after warm-up and horizon filtering".to_string());
    }

    // 3. Chronological split: earlier rows fit the estimators; later rows
    // calibrate probabilities; the final segment is reserved and not used here.
    // Split only the interval where features and full 60-minute labels exist.
    // Reserve fixed 120-bar gaps (60-minute horizon + 60-minute embargo)
    // before calibration and before the untouched final test segment.
    let development_events = usable_events - 2 * split_gap;
    let fit_len = development_events * 70 / 100;
    let calibration_len = development_events * 15 / 100;
    let fit_cut = first_event + fit_len;
    let calibration_start = fit_cut + split_gap;
    let calibration_end = calibration_start + calibration_len;
    let final_test_start = calibration_end + split_gap;

    let mut fit_rows = Vec::new();
    let mut calibration_rows = Vec::new();
    for row in rows {
        if row.event_idx < fit_cut && row.label_end_idx + EMBARGO_BARS <= calibration_start {
            fit_rows.push(row.sample);
        } else if row.event_idx >= calibration_start
            && row.event_idx < calibration_end
            && row.label_end_idx + EMBARGO_BARS <= final_test_start
        {
            calibration_rows.push(row.sample);
        }
        // Events in the final test segment are deliberately not passed to fit.
    }

    if fit_rows.len() < 100 || calibration_rows.len() < 30 {
        return Err(format!(
            "not enough rows after purging: fit={}, calibration={}",
            fit_rows.len(),
            calibration_rows.len()
        ));
    }

    // 4. Fit Logistic Regression + Ridge on the earlier period, then fit
    // temperature calibration only on the later, disjoint calibration period.
    let model_config = ModelConfig::default();
    let mut model = IntradayModel::fit(&fit_rows, &model_config)?;
    model.fit_calibrator(&calibration_rows)?;

    // 5. Score the newest fully warmed-up snapshot. No order is sent here.
    let latest = compute_latest_features_v1(&bars)?;
    let policy = Policy {
        min_win_probability: 0.55,
        min_expected_net_return_bps: 3.0,
    };
    let long = model.predict_v1(latest, Side::Long)?;
    let short = model.predict_v1(latest, Side::Short)?;
    println!(
        "LONG: P_WIN={:.3}, P_NEUTRAL={:.3}, P_LOSS={:.3}, expected_net={:.2} bps",
        long.p_win, long.p_neutral, long.p_loss, long.expected_net_return_bps
    );
    println!(
        "SHORT: P_WIN={:.3}, P_NEUTRAL={:.3}, P_LOSS={:.3}, expected_net={:.2} bps",
        short.p_win, short.p_neutral, short.p_loss, short.expected_net_return_bps
    );
    println!(
        "LONG gates: P_WIN={}, net_EV={}",
        long.p_win >= policy.min_win_probability,
        long.expected_net_return_bps >= policy.min_expected_net_return_bps
    );
    println!(
        "SHORT gates: P_WIN={}, net_EV={}",
        short.p_win >= policy.min_win_probability,
        short.expected_net_return_bps >= policy.min_expected_net_return_bps
    );
    println!(
        "entry thresholds: P_WIN >= {:.3}, expected_net >= {:.2} bps",
        policy.min_win_probability, policy.min_expected_net_return_bps
    );
    let signal = choose_signal_v1(&model, latest, policy)?;
    println!("signal: {signal:?}");
    println!(
        "fit rows: {}, calibration rows: {}",
        fit_rows.len(),
        calibration_rows.len()
    );
    Ok(())
}

fn load_minute_bars_csv(path: &str) -> Result<Vec<Kline>, String> {
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
        bars.push(Kline {
            open_time_ms,
            close_time_ms,
            open: parse(1, "open")?,
            high: parse(2, "high")?,
            low: parse(3, "low")?,
            close: parse(4, "close")?,
            base_volume: parse(5, "base_volume")?,
            quote_volume,
        });
    }
    Ok(bars)
}

fn build_labeled_rows(
    bars: &[Kline],
    features: &[Option<intraday_model::FeaturesV1>],
    cfg: CostAndBarrierConfig,
) -> Result<Vec<LabeledRow>, String> {
    if bars.len() != features.len() {
        return Err("bars/features length mismatch".to_string());
    }
    let mut rows = Vec::new();
    let last_decision = bars.len() - HORIZON_BARS - 1;

    for event_idx in (MIN_REQUIRED_BARS - 1)..=last_decision {
        let feature_row = match features[event_idx] {
            Some(value) => value,
            None => continue,
        };
        for side in [Side::Long, Side::Short] {
            if let Some((label_end_idx, outcome, net_return_bps)) =
                simulate_candidate(bars, event_idx, feature_row, side, cfg)
            {
                rows.push(LabeledRow {
                    event_idx,
                    label_end_idx,
                    sample: TrainingSample {
                        features: feature_row.to_vec(),
                        side,
                        outcome,
                        net_return_bps,
                    },
                });
            }
        }
    }
    Ok(rows)
}

/// OHLCV-only barrier proxy. If TP and SL are both touched in one candle,
/// count SL first. A tick/order-book simulator should replace this in production.
fn simulate_candidate(
    bars: &[Kline],
    event_idx: usize,
    features: intraday_model::FeaturesV1,
    side: Side,
    cfg: CostAndBarrierConfig,
) -> Option<(usize, Outcome, f64)> {
    let deadline = event_idx + HORIZON_BARS;
    if deadline >= bars.len() {
        return None;
    }

    // Decision is made at the close of event_idx; next candle open is the
    // conservative 1-minute entry proxy when only OHLCV is available.
    let entry = bars[event_idx + 1].open;
    let decision_close = bars[event_idx].close;
    let atr_price = decision_close * features.atr14_pct_bps / 10_000.0;
    let tp_dist = (cfg.take_profit_atr * atr_price)
        .max((cfg.round_trip_cost_bps + cfg.edge_buffer_bps) * entry / 10_000.0);
    let sl_dist = (cfg.stop_loss_atr * atr_price)
        .max((cfg.round_trip_cost_bps + cfg.stop_buffer_bps) * entry / 10_000.0);

    let (tp_price, sl_price) = match side {
        Side::Long => (entry + tp_dist, entry - sl_dist),
        Side::Short => (entry - tp_dist, entry + sl_dist),
    };

    let mut exit_idx = deadline;
    let mut exit_price = bars[deadline].close;
    'path: for i in (event_idx + 1)..=deadline {
        let bar = bars[i];
        let (hit_tp, hit_sl) = match side {
            Side::Long => (bar.high >= tp_price, bar.low <= sl_price),
            Side::Short => (bar.low <= tp_price, bar.high >= sl_price),
        };

        if hit_sl {
            exit_idx = i;
            // A gap through the stop is filled at the worse open.
            exit_price = match side {
                Side::Long if bar.open < sl_price => bar.open,
                Side::Short if bar.open > sl_price => bar.open,
                _ => sl_price,
            };
            break 'path;
        }
        if hit_tp {
            exit_idx = i;
            // Conservative limit-touch proxy: no favorable gap improvement.
            exit_price = tp_price;
            break 'path;
        }
    }

    let raw_mid_return_bps = (exit_price / entry - 1.0) * 10_000.0;
    let side_sign = match side {
        Side::Long => 1.0,
        Side::Short => -1.0,
    };
    let gross_side_return_bps = side_sign * raw_mid_return_bps;
    // COST_RT_BPS must include both fees, spread crossing, slippage/impact,
    // and any conservative funding allowance for the fixed reference notional.
    let net_return_bps = gross_side_return_bps - cfg.round_trip_cost_bps;
    let outcome = classify_outcome(
        side,
        raw_mid_return_bps,
        net_return_bps,
        cfg.round_trip_cost_bps,
        cfg.edge_buffer_bps,
    );
    Some((exit_idx, outcome, net_return_bps))
}
