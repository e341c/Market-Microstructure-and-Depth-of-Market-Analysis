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

use std::env;

use market_microstructure::file::load_minute_bars_csv;
use market_microstructure::intraday_features::{MIN_REQUIRED_BARS, compute_features_v1_all};
use market_microstructure::intraday_flow::validate_flow_against_klines;
use market_microstructure::intraday_flow::{
    FLOW_FEATURE_COUNT, FLOW_FEATURE_NAMES, TOP_DECILE_Z, align_flow_to_bars,
    compute_flow_features_all, load_flow_csv,
};
use market_microstructure::intraday_model::{
    FEATURE_NAMES_V1, FeaturesV1, IntradayModel, ModelConfig, Outcome, Policy, Side,
    TrainingSample, choose_signal, classify_outcome,
};
use market_microstructure::kline::Kline;

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
    let ohlcv_csv_path = env::args()
        .nth(1)
        .ok_or_else(|| "usage: rust_train_model_example <ohlcv.csv>".to_string())?;
    // let agg_trade_csv_path = env::args()
    //     .nth(2)
    //     .ok_or_else(|| "usage: rust_train_model_example <aggTrade.csv>".to_string())?;
    let cost_rt_bps: f64 = env::var("COST_RT_BPS")
        .map_err(|_| "set COST_RT_BPS to fees + spread + slippage + other costs".to_string())?
        .parse()
        .map_err(|_| "COST_RT_BPS must be a number".to_string())?;

    let bars = load_minute_bars_csv(&ohlcv_csv_path)?;
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

    // Путь к минутным агрегатам aggTrades (результат aggregate_flow)
    let flow_path = env::var("FLOW_CSV").map_err(|_| "set FLOW_CSV=flow_1m.csv".to_string())?;
    let flow_map = load_flow_csv(&flow_path)?;

    println!(
        "Training model started with COST_RT_BPS={} and train data is {}, flow path is {}",
        cost_rt_bps, ohlcv_csv_path, flow_path
    );

    validate_flow_against_klines(&ohlcv_csv_path, &flow_map)?;
    // Агрегаты минут, выровненные по свечам (None - минуты нет в aggTrades)
    let flow_aligned = align_flow_to_bars(&bars, &flow_map);
    let covered = flow_aligned.iter().filter(|f| f.is_some()).count();
    println!(
        "flow coverage: {covered}/{} минут ({:.1}%)",
        bars.len(),
        100.0 * covered as f64 / bars.len() as f64
    );
    let flow_feats = compute_flow_features_all(&bars, &features, &flow_aligned)?;

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
    let rows = build_labeled_rows(&bars, &features, &flow_feats, cost_cfg)?;
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

    print_flow_ic_scan("FIT", &bars, &flow_feats, first_event, fit_cut);
    print_flow_ic_scan(
        "TEST",
        &bars,
        &flow_feats,
        final_test_start,
        last_decision + 1,
    );

    // После построения features и границ сплита, до обучения:
    print_feature_ic("FIT", &bars, &features, first_event, fit_cut);
    print_feature_ic(
        "TEST",
        &bars,
        &features,
        final_test_start,
        last_decision + 1,
    );

    // Скан на обучающем и отложенном участках: сильный край должен быть виден на обоих с одним знаком
    scan_horizons("FIT", &bars, &features, first_event, fit_cut, cost_rt_bps);
    scan_horizons(
        "TEST",
        &bars,
        &features,
        final_test_start,
        last_decision + 1,
        cost_rt_bps,
    );

    let mut fit_rows = Vec::new();
    let mut calibration_rows = Vec::new();
    let mut test_rows = Vec::new();
    for row in rows {
        if row.event_idx < fit_cut && row.label_end_idx + EMBARGO_BARS <= calibration_start {
            fit_rows.push(row.sample);
        } else if row.event_idx >= calibration_start
            && row.event_idx < calibration_end
            && row.label_end_idx + EMBARGO_BARS <= final_test_start
        {
            calibration_rows.push(row.sample);
        } else if row.event_idx >= final_test_start {
            // Held-out chronological segment: never used for fitting/calibration.
            test_rows.push(row.sample);
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
    // После model.fit_calibrator(...):
    print_decile_table("TEST", &test_rows, &model, Side::Long)?;
    print_decile_table("TEST", &test_rows, &model, Side::Short)?;

    for side in [Side::Long, Side::Short] {
        // Для WinMinusLoss и ExpectedNet монотонность net и положительная разность — признак навыка
        print_decile_table_by("TEST", &test_rows, &model, side, DecileScore::WinMinusLoss)?;
        print_decile_table_by("TEST", &test_rows, &model, side, DecileScore::ExpectedNet)?;
    }
    print_sample_metrics("FIT (in-sample)", &fit_rows, &model)?;
    print_sample_metrics("CALIBRATION", &calibration_rows, &model)?;
    print_sample_metrics("HELD-OUT TEST", &test_rows, &model)?;
    let model_path = env::var("MODEL_OUT").unwrap_or_else(|_| "intraday_model_v4.txt".to_string());
    model.save_to_file(&model_path)?;
    println!("saved trained model: {model_path}");

    // 5. Build the same V2 feature vector used during training:
    //    23 OHLCV features followed by FLOW_FEATURE_COUNT flow features.
    //    Use the latest row for which both blocks are available.
    let latest_idx = (0..bars.len())
        .rev()
        .find(|&i| features[i].is_some() && flow_feats[i].is_some())
        .ok_or_else(|| "no latest row has both OHLCV and flow features".to_string())?;
    let latest_v1 =
        features[latest_idx].ok_or_else(|| "latest OHLCV features are missing".to_string())?;
    let latest_flow =
        flow_feats[latest_idx].ok_or_else(|| "latest flow features are missing".to_string())?;
    let mut latest_full = latest_v1.to_vec();
    latest_full.extend_from_slice(&latest_flow);

    // No order is sent here; this only scores the latest complete candle.

    // 5. Score the newest fully warmed-up snapshot. No order is sent here.
    // let latest = compute_latest_features_v1(&bars)?;
    let policy = Policy {
        min_win_probability: 0.55,
        min_expected_net_return_bps: 3.0,
    };
    // let long = model.predict_v1(latest, Side::Long)?;
    // let short = model.predict_v1(latest, Side::Short)?;

    let long = model.predict(&latest_full, Side::Long)?;
    let short = model.predict(&latest_full, Side::Short)?;
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
    // let signal = choose_signal_v1(&model, latest, policy)?;
    let signal = choose_signal(&model, &latest_full, policy)?;
    println!("signal: {signal:?}");
    println!(
        "fit rows: {}, calibration rows: {}",
        fit_rows.len(),
        calibration_rows.len()
    );
    Ok(())
}

/// Reports label frequencies plus actual and predicted net-return distributions.
/// The held-out test set is diagnostic only; it is never used to fit the model.
fn print_sample_metrics(
    name: &str,
    samples: &[TrainingSample],
    model: &IntradayModel,
) -> Result<(), String> {
    if samples.is_empty() {
        println!("{name}: no rows");
        return Ok(());
    }

    let mut wins = 0usize;
    let mut neutral = 0usize;
    let mut losses = 0usize;
    let mut actual = Vec::with_capacity(samples.len());
    let mut predicted = Vec::with_capacity(samples.len());

    for row in samples {
        match row.outcome {
            Outcome::Win => wins += 1,
            Outcome::Neutral => neutral += 1,
            Outcome::Loss => losses += 1,
        }
        actual.push(row.net_return_bps);
        predicted.push(
            model
                .predict(&row.features, row.side)?
                .expected_net_return_bps,
        );
    }

    println!(
        "{name}: rows={}, labels WIN/NEUTRAL/LOSS={}/{}/{}, actual_net_bps: {}, predicted_net_bps: {}",
        samples.len(),
        wins,
        neutral,
        losses,
        summarize(&mut actual),
        summarize(&mut predicted),
    );
    Ok(())
}

fn summarize(values: &mut [f64]) -> String {
    values.sort_by(|a, b| a.total_cmp(b));
    let n = values.len();
    let mean = values.iter().sum::<f64>() / n as f64;
    let q = |p: f64| -> f64 {
        let index = ((n - 1) as f64 * p).round() as usize;
        values[index]
    };
    format!(
        "mean={mean:.2}, p10={:.2}, p25={:.2}, median={:.2}, p75={:.2}, p90={:.2}, min={:.2}, max={:.2}",
        q(0.10),
        q(0.25),
        q(0.50),
        q(0.75),
        q(0.90),
        values[0],
        values[n - 1]
    )
}

fn build_labeled_rows(
    bars: &[Kline],
    features: &[Option<FeaturesV1>],
    flow_feats: &[Option<[f64; FLOW_FEATURE_COUNT]>],
    cfg: CostAndBarrierConfig,
) -> Result<Vec<LabeledRow>, String> {
    if bars.len() != features.len() || bars.len() != flow_feats.len() {
        return Err("bars/features/flow length mismatch".to_string());
    }
    let mut rows = Vec::new();
    let last_decision = bars.len() - HORIZON_BARS - 1;

    for event_idx in (MIN_REQUIRED_BARS - 1)..=last_decision {
        let feature_row = match features[event_idx] {
            Some(value) => value,
            None => continue,
        };
        // Нет потоковых признаков (пропуск aggTrades) - событие пропускаем
        let flow_row = match flow_feats[event_idx] {
            Some(value) => value,
            None => continue,
        };
        // Полный вектор признаков V2: 23 признака V1 + 8 потоковых
        let mut full = feature_row.to_vec();
        full.extend_from_slice(&flow_row);
        for side in [Side::Long, Side::Short] {
            if let Some((label_end_idx, outcome, net_return_bps)) =
                simulate_candidate(bars, event_idx, feature_row, side, cfg)
            {
                rows.push(LabeledRow {
                    event_idx,
                    label_end_idx,
                    sample: TrainingSample {
                        features: full.clone(), // V2-вектор; барьеры считались по ATR из V1
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
    features: FeaturesV1,
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

/// Ранги элементов вектора (0..n-1) по возрастанию значения.
/// Нужны для корреляции Спирмена: Spearman(x, y) = Pearson(rank(x), rank(y)).
fn ranks(values: &[f64]) -> Vec<f64> {
    // Индексы, отсортированные по значению элементов
    let mut idx: Vec<usize> = (0..values.len()).collect();
    idx.sort_by(|&a, &b| values[a].total_cmp(&values[b]));
    // Ранг элемента = его позиция в отсортированном порядке
    let mut r = vec![0.0; values.len()];
    for (rank, &i) in idx.iter().enumerate() {
        r[i] = rank as f64;
    }
    r
}

/// Корреляция Пирсона: corr(x, y) = cov(x, y) / (sigma_x * sigma_y).
/// Здесь через суммы: sxy / sqrt(sxx * syy), где sxy = sum(dx*dy), sxx = sum(dx^2), syy = sum(dy^2).
fn pearson(x: &[f64], y: &[f64]) -> f64 {
    let n = x.len() as f64;
    let mx = x.iter().sum::<f64>() / n; // среднее x
    let my = y.iter().sum::<f64>() / n; // среднее y
    let (mut sxy, mut sxx, mut syy) = (0.0, 0.0, 0.0);
    for i in 0..x.len() {
        let dx = x[i] - mx; // отклонение x от среднего
        let dy = y[i] - my; // отклонение y от среднего
        sxy += dx * dy;
        sxx += dx * dx;
        syy += dy * dy;
    }
    if sxx <= 0.0 || syy <= 0.0 {
        0.0
    } else {
        sxy / (sxx * syy).sqrt()
    }
}

/// Печатает Spearman IC каждого признака с ФОРВАРДНОЙ ВАЛОВОЙ доходностью (без издержек и барьеров)
/// на отрезке событий [start, end). Показывает, есть ли у признаков хоть какая-то предсказательность.
/// Форвардная доходность = 1e4 * ln(close[t+H] / open[t+1]) в б.п., вход по open следующей свечи.
fn print_feature_ic(
    name: &str,
    bars: &[Kline],
    features: &[Option<FeaturesV1>],
    start: usize,
    end: usize,
) {
    // xs[j] - значения j-го признака, ys - форвардные доходности (выровнены по строкам)
    let mut xs: Vec<Vec<f64>> = vec![Vec::new(); FEATURE_NAMES_V1.len()];
    let mut ys: Vec<f64> = Vec::new();
    for t in start..end {
        if t + HORIZON_BARS >= bars.len() {
            break; // нет полного горизонта вперёд
        }
        let Some(f) = features[t] else { continue };
        let entry = bars[t + 1].open;
        let exit = bars[t + HORIZON_BARS].close;
        ys.push(10_000.0 * (exit / entry).ln()); // форвардная доходность, б.п.
        for (j, v) in f.to_vec().into_iter().enumerate() {
            xs[j].push(v);
        }
    }
    if ys.len() < 100 {
        println!("IC {name}: слишком мало строк ({})", ys.len());
        return;
    }
    let ry = ranks(&ys);
    // Окна по 60 минут перекрываются на шаге 1 минута, поэтому независимых наблюдений ~ N / H
    let n_eff = ys.len() as f64 / HORIZON_BARS as f64;
    // Стандартная ошибка корреляции при нулевой истинной связи ~ 1 / sqrt(n_eff)
    let se = 1.0 / n_eff.sqrt();
    println!(
        "IC {name}: rows={}, n_eff~{:.0}, SE~{:.3} (|IC| < 2*SE - шум)",
        ys.len(),
        n_eff,
        se
    );
    for (j, col) in xs.iter().enumerate() {
        let ic = pearson(&ranks(col), &ry); // Spearman IC признака j
        println!("  {:<26} IC={:+.4}", FEATURE_NAMES_V1[j], ic);
    }
}

/// Таблица по децилям предсказанного P_WIN для выбранной стороны:
/// если модель что-то умеет, фактическая доля Win и средний net должны расти от 1-го дециля к 10-му.
/// Плоская таблица = сигнала нет.
fn print_decile_table(
    name: &str,
    samples: &[TrainingSample],
    model: &IntradayModel,
    side: Side,
) -> Result<(), String> {
    // Строки: (предсказанный p_win, 1.0 если Win иначе 0.0, фактический net в б.п.)
    let mut rows: Vec<(f64, f64, f64)> = Vec::new();
    for s in samples.iter().filter(|s| s.side == side) {
        let p = model.predict(&s.features, s.side)?;
        let is_win = if s.outcome == Outcome::Win { 1.0 } else { 0.0 };
        rows.push((p.p_win, is_win, s.net_return_bps));
    }
    if rows.len() < 100 {
        println!("{name} {side:?}: мало строк");
        return Ok(());
    }
    rows.sort_by(|a, b| a.0.total_cmp(&b.0)); // сортировка по предсказанному p_win
    let n = rows.len();
    println!("{name} {side:?}: дециль | mean_pred_pwin | actual_win_rate | mean_net_bps");
    for d in 0..10 {
        // границы дециля: d-я десятая часть отсортированных строк
        let lo = n * d / 10;
        let hi = n * (d + 1) / 10;
        let cnt = (hi - lo) as f64;
        let mean_p = rows[lo..hi].iter().map(|r| r.0).sum::<f64>() / cnt; // средний предсказанный p_win
        let win_rate = rows[lo..hi].iter().map(|r| r.1).sum::<f64>() / cnt; // фактическая доля Win
        let mean_net = rows[lo..hi].iter().map(|r| r.2).sum::<f64>() / cnt; // средний фактический net
        println!(
            "  {:>2} | {:.3} | {:.3} | {:+.2}",
            d + 1,
            mean_p,
            win_rate,
            mean_net
        );
    }
    Ok(())
}

/// Горизонты (в минутных барах), по которым ищем лучшую предсказательность.
const SCAN_HORIZONS: [usize; 5] = [15, 30, 60, 240, 720];

/// Среднее z верхних 10% стандартного нормального распределения:
/// E[z | z > 1.2816] = phi(1.2816) / 0.10 = 0.1755 / 0.10 = 1.755.
/// Нужна для грубой оценки ожидаемой доходности в верхнем дециле сигнала.
// const TOP_DECILE_Z: f64 = 1.755;

/// Для каждого горизонта H считает Spearman IC всех признаков с форвардной валовой доходностью
/// на отрезке событий [start, end) и печатает лучший признак, σ_H и оценку края топ-дециля
/// в сравнении с издержками. Это диагностика ДО обучения: если оценка края << cost_rt_bps,
/// то никакая модель на этих признаках и горизонте не будет прибыльной.
fn scan_horizons(
    name: &str,
    bars: &[Kline],
    features: &[Option<FeaturesV1>],
    start: usize,
    end: usize,
    cost_rt_bps: f64,
) {
    for &h in SCAN_HORIZONS.iter() {
        // xs[j] - значения признака j; ys - форвардная доходность за h баров в bps
        let mut xs: Vec<Vec<f64>> = vec![Vec::new(); FEATURE_NAMES_V1.len()];
        let mut ys: Vec<f64> = Vec::new();
        for t in start..end {
            if t + h >= bars.len() {
                break; // нет полного горизонта вперёд
            }
            let Some(f) = features[t] else { continue };
            // Вход по open следующей свечи, выход по close через h баров:
            // ret_bps = 1e4 * ln(close[t+h] / open[t+1])
            ys.push(10_000.0 * (bars[t + h].close / bars[t + 1].open).ln());
            for (j, v) in f.to_vec().into_iter().enumerate() {
                xs[j].push(v);
            }
        }
        if ys.len() < 1000 {
            println!("SCAN {name} H={h}: мало строк ({})", ys.len());
            continue;
        }
        let n = ys.len() as f64;
        // σ_H = sqrt( sum((y - mean)^2) / n ) - разброс доходности за горизонт
        let mean_y = ys.iter().sum::<f64>() / n;
        let sigma_h = (ys.iter().map(|y| (y - mean_y).powi(2)).sum::<f64>() / n).sqrt();
        let ry = ranks(&ys);
        // Лучший признак по |IC|
        let mut best_j = 0usize;
        let mut best_ic = 0.0f64;
        for (j, col) in xs.iter().enumerate() {
            let ic = pearson(&ranks(col), &ry); // Spearman IC признака j
            if ic.abs() > best_ic.abs() {
                best_ic = ic;
                best_j = j;
            }
        }
        // Независимых наблюдений ~ n / h (окна перекрываются), SE ~ 1 / sqrt(n_eff)
        let se = 1.0 / (n / h as f64).sqrt();
        // Оценка валового края верхнего дециля: |IC| * σ_H * 1.755
        let edge_bps = best_ic.abs() * sigma_h * TOP_DECILE_Z;
        println!(
            "SCAN {name} H={h}: σ_H={sigma_h:.1} bps, лучший признак={} IC={best_ic:+.4} (2·SE={:.3}), \
             край топ-дециля≈{edge_bps:.1} bps vs издержки {cost_rt_bps:.1} bps",
            FEATURE_NAMES_V1[best_j],
            2.0 * se
        );
    }
}

/// По какому скору делить строки на децили.
#[derive(Clone, Copy, Debug)]
enum DecileScore {
    /// p_win: вероятность касания TP до SL. Включает волатильность, поэтому для оценки направления не годится.
    PWin,
    /// p_win - p_loss: направленный скор. Волатильность поднимает оба слагаемых и сокращается.
    WinMinusLoss,
    /// Предсказанный регрессором ожидаемый net (б.п.).
    ExpectedNet,
}

/// Таблица по децилям выбранного скора для одной стороны. Для каждого дециля печатает средний скор,
/// долю Win, долю Loss, средний фактический net и наивную SE среднего.
/// SE считается как std / sqrt(n) и ЗАНИЖЕНА: окна по 60 мин перекрываются, наблюдения не независимы.
/// В конце печатает разность среднего net верхнего и нижнего децилей. Если направленный навык есть,
/// она должна быть заметно положительной и таблица монотонной.
fn print_decile_table_by(
    name: &str,
    samples: &[TrainingSample],
    model: &IntradayModel,
    side: Side,
    score_kind: DecileScore,
) -> Result<(), String> {
    // Строки: (скор, 1.0 если Win, 1.0 если Loss, фактический net в б.п.)
    let mut rows: Vec<(f64, f64, f64, f64)> = Vec::new();
    for s in samples.iter().filter(|s| s.side == side) {
        let p = model.predict(&s.features, s.side)?;
        // Скор для сортировки: p_win, либо p_win - p_loss, либо ожидаемый net
        let score = match score_kind {
            DecileScore::PWin => p.p_win,
            DecileScore::WinMinusLoss => p.p_win - p.p_loss,
            DecileScore::ExpectedNet => p.expected_net_return_bps,
        };
        let is_win = if s.outcome == Outcome::Win { 1.0 } else { 0.0 };
        let is_loss = if s.outcome == Outcome::Loss { 1.0 } else { 0.0 };
        rows.push((score, is_win, is_loss, s.net_return_bps));
    }
    if rows.len() < 100 {
        println!("{name} {side:?} {score_kind:?}: мало строк");
        return Ok(());
    }
    rows.sort_by(|a, b| a.0.total_cmp(&b.0)); // сортировка по скору
    let n = rows.len();
    println!(
        "{name} {side:?} по {score_kind:?}: дециль | mean_score | win | loss | mean_net_bps | se"
    );

    let mut decile_net: Vec<f64> = Vec::with_capacity(10);
    for d in 0..10 {
        // Границы дециля: d-я десятая часть отсортированных строк
        let lo = n * d / 10;
        let hi = n * (d + 1) / 10;
        let cnt = (hi - lo) as f64;
        let slice = &rows[lo..hi];
        let mean_score = slice.iter().map(|r| r.0).sum::<f64>() / cnt; // средний скор
        let win_rate = slice.iter().map(|r| r.1).sum::<f64>() / cnt; // доля Win
        let loss_rate = slice.iter().map(|r| r.2).sum::<f64>() / cnt; // доля Loss
        let mean_net = slice.iter().map(|r| r.3).sum::<f64>() / cnt; // средний net
        // Выборочная дисперсия net: var = sum((x - mean)^2) / n
        let var = slice.iter().map(|r| (r.3 - mean_net).powi(2)).sum::<f64>() / cnt;
        // Наивная стандартная ошибка среднего: se = sqrt(var / n)
        let se = (var / cnt).sqrt();
        println!(
            "  {:>2} | {:+.3} | {:.3} | {:.3} | {:+.2} | {:.2}",
            d + 1,
            mean_score,
            win_rate,
            loss_rate,
            mean_net,
            se
        );
        decile_net.push(mean_net);
    }
    // Разность средних net между верхним и нижним децилем: мера направленного навыка
    println!(
        "  top-bottom spread: {:+.2} bps",
        decile_net[9] - decile_net[0]
    );
    Ok(())
}

/// Горизонты скана для потоковых признаков, минуты: поток обычно работает на коротких горизонтах.
const FLOW_SCAN_HORIZONS: [usize; 5] = [1, 5, 15, 30, 60];

/// Для каждого горизонта H печатает Spearman IC каждого потокового признака с форвардной
/// валовой доходностью ret = 1e4 * ln(close[t+H] / open[t+1]) и оценку края топ-дециля:
/// edge ≈ |IC| * σ_H * 1.755. Сравнивайте edge с издержками круга (4-11 б.п.).
fn print_flow_ic_scan(
    name: &str,
    bars: &[Kline],
    flow_feats: &[Option<[f64; FLOW_FEATURE_COUNT]>],
    start: usize,
    end: usize,
) {
    for &h in FLOW_SCAN_HORIZONS.iter() {
        let mut xs: Vec<Vec<f64>> = vec![Vec::new(); FLOW_FEATURE_COUNT];
        let mut ys: Vec<f64> = Vec::new();
        for t in start..end {
            if t + h >= bars.len() {
                break; // нет полного горизонта
            }
            let Some(f) = flow_feats[t] else { continue };
            ys.push(10_000.0 * (bars[t + h].close / bars[t + 1].open).ln());
            for j in 0..FLOW_FEATURE_COUNT {
                xs[j].push(f[j]);
            }
        }
        if ys.len() < 1000 {
            println!("FLOW {name} H={h}: мало строк ({})", ys.len());
            continue;
        }
        let n = ys.len() as f64;
        let mean_y = ys.iter().sum::<f64>() / n;
        // σ_H = sqrt(sum((y - mean)^2) / n)
        let sigma = (ys.iter().map(|y| (y - mean_y).powi(2)).sum::<f64>() / n).sqrt();
        // Независимых наблюдений ~ n / H (окна перекрываются), SE ~ 1 / sqrt(n_eff)
        let se = 1.0 / (n / h as f64).sqrt();
        println!(
            "FLOW {name} H={h}: rows={}, σ_H={sigma:.1} bps, 2·SE={:.3}",
            ys.len(),
            2.0 * se
        );
        let ry = ranks(&ys);
        for j in 0..FLOW_FEATURE_COUNT {
            let ic = pearson(&ranks(&xs[j]), &ry);
            println!(
                "  {:<28} IC={ic:+.4} edge≈{:.2} bps",
                FLOW_FEATURE_NAMES[j],
                ic.abs() * sigma * TOP_DECILE_Z
            );
        }
    }
}
