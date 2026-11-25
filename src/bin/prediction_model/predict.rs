//! Inference-only binary: loads a fitted model; does not train.
use market_microstructure::file::load_minute_bars_csv;
use market_microstructure::intraday_features::compute_latest_features_v1;
use market_microstructure::intraday_model::{IntradayModel, Policy, Side, choose_signal_v1};
use market_microstructure::kline::Kline;
use std::{
    env,
    fs::File,
    io::{BufRead, BufReader},
};

fn main() -> Result<(), String> {
    let a: Vec<String> = env::args().collect();
    if a.len() != 3 {
        return Err("usage: rust_predict_model <model.txt> <klines.csv>".into());
    }
    let model = IntradayModel::load_from_file(&a[1], 23)?;
    let bars = load_minute_bars_csv(&a[2])?;
    let features = compute_latest_features_v1(&bars)?;
    let policy = Policy {
        min_win_probability: 0.55,
        min_expected_net_return_bps: 3.0,
    };
    let long = model.predict_v1(features, Side::Long)?;
    let short = model.predict_v1(features, Side::Short)?;
    println!(
        "LONG: P_WIN={:.3}, P_NEUTRAL={:.3}, P_LOSS={:.3}, expected_net={:.2} bps",
        long.p_win, long.p_neutral, long.p_loss, long.expected_net_return_bps
    );
    println!(
        "SHORT: P_WIN={:.3}, P_NEUTRAL={:.3}, P_LOSS={:.3}, expected_net={:.2} bps",
        short.p_win, short.p_neutral, short.p_loss, short.expected_net_return_bps
    );
    println!(
        "LONG gates: P_WIN={} ({} >= {}), net_EV={} ({} >= {} bps)",
        long.p_win >= policy.min_win_probability,
        long.p_win,
        policy.min_win_probability,
        long.expected_net_return_bps >= policy.min_expected_net_return_bps,
        long.expected_net_return_bps,
        policy.min_expected_net_return_bps
    );
    println!(
        "SHORT gates: P_WIN={} ({} >= {}), net_EV={} ({} >= {} bps)",
        short.p_win >= policy.min_win_probability,
        short.p_win,
        policy.min_win_probability,
        short.expected_net_return_bps >= policy.min_expected_net_return_bps,
        short.expected_net_return_bps,
        policy.min_expected_net_return_bps
    );
    println!(
        "entry thresholds: P_WIN >= {:.3}, expected_net >= {:.2} bps",
        policy.min_win_probability, policy.min_expected_net_return_bps
    );
    println!("signal: {:?}", choose_signal_v1(&model, features, policy)?);
    Ok(())
}
// fn load(path: &str) -> Result<Vec<Kline>, String> {
//     let f = File::open(path).map_err(|e| e.to_string())?;
//     let mut lines = BufReader::new(f).lines();
//     lines
//         .next()
//         .ok_or("CSV has no header")?
//         .map_err(|e| e.to_string())?;
//     let mut out = Vec::new();
//     for (i, l) in lines.enumerate() {
//         let s = l.map_err(|e| e.to_string())?;
//         if s.trim().is_empty() {
//             continue;
//         }
//         let c: Vec<&str> = s.trim().split(',').collect();
//         if c.len() != 12 {
//             return Err(format!("row {}: expected 12 columns", i + 2));
//         }
//         let f64c = |n: usize| {
//             c[n].parse::<f64>()
//                 .map_err(|_| format!("row {}: invalid column {}", i + 2, n))
//         };
//         out.push(Kline {
//             open_time_ms: c[0]
//                 .parse()
//                 .map_err(|_| format!("row {}: invalid open_time", i + 2))?,
//             open: f64c(1)?,
//             high: f64c(2)?,
//             low: f64c(3)?,
//             close: f64c(4)?,
//             base_volume: f64c(5)?,
//             close_time_ms: c[6]
//                 .parse()
//                 .map_err(|_| format!("row {}: invalid close_time", i + 2))?,
//             quote_volume: Some(f64c(7)?),
//         });
//     }
//     Ok(out)
// }
