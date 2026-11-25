//! Скользящий inference/backtest для модели схемы V2 (23 признака OHLCV + 8 потоковых из aggTrades).
//! Файл модели должен быть обучен только на периоде до проверяемого периода.
//!
//! Запуск:
//!   cargo run --release --bin backtest <model.txt> <klines.csv> [flow_1m.csv]
//! Путь к flow_1m.csv (результат aggregate_flow) задаётся третьим аргументом
//! или переменной окружения FLOW_CSV. Нужна переменная COST_RT_BPS.
//! Минутные агрегаты aggTrades должны покрывать период проверяемых свечей:
//! отчёт сверки в начале запуска показывает покрытие.

use market_microstructure::file::load_minute_bars_csv;
use market_microstructure::intraday_features::{MIN_REQUIRED_BARS, compute_features_v1_all};
use market_microstructure::intraday_flow::{
    FLOW_FEATURE_COUNT, align_flow_to_bars, compute_flow_features_all, load_flow_csv,
    validate_flow_against_klines,
};
use market_microstructure::intraday_model::{
    FEATURE_NAMES_V1, FeaturesV1, IntradayModel, Outcome, Policy, Side, Signal, choose_signal,
    classify_outcome,
};
use market_microstructure::kline::Kline;
use std::{
    env,
    fs::File,
    io::{BufRead, BufReader},
};

/// Максимальное время удержания позиции в минутных барах (таймаут).
const HORIZON_BARS: usize = 60;
/// Множитель ATR для тейк-профита: tp_dist = max(TAKE_PROFIT_ATR * ATR, пол по издержкам).
const TAKE_PROFIT_ATR: f64 = 1.5;
/// Множитель ATR для стоп-лосса: sl_dist = max(STOP_LOSS_ATR * ATR, пол по издержкам).
const STOP_LOSS_ATR: f64 = 1.0;
/// Запас над издержками для тейк-профита, б.п. (пол TP = cost + EDGE_BUFFER_BPS).
const EDGE_BUFFER_BPS: f64 = 2.0;
/// Запас над издержками для стоп-лосса, б.п. (пол SL = cost + STOP_BUFFER_BPS).
const STOP_BUFFER_BPS: f64 = 2.0;

/// Причина выхода из сделки.
#[derive(Clone, Copy, Debug)]
enum ExitReason {
    TakeProfit,
    StopLoss,
    Timeout,
}

/// Результат одной смоделированной сделки.
struct TradeResult {
    /// Чистая доходность после издержек, б.п.
    net_return_bps: f64,
    /// Класс исхода (Win / Neutral / Loss) по тем же правилам, что и при обучении.
    outcome: Outcome,
    /// Чем закончилась сделка: TP, SL или таймаут.
    exit_reason: ExitReason,
    /// Индекс свечи выхода.
    exit_idx: usize,
}

/// One hypothetical side at one test timestamp, recorded even when policy rejects it.
#[derive(Clone, Copy)]
struct ScoredCandidate {
    predicted_net_bps: f64,
    realized_net_bps: f64,
}

fn main() -> Result<(), String> {
    let args: Vec<String> = env::args().collect();

    if args.len() < 3 || args.len() > 4 {
        return Err("usage: backtest <model.txt> <klines.csv> [flow_1m.csv]".into());
    }

    // Путь к минутным агрегатам aggTrades: 3-й аргумент или переменная FLOW_CSV
    let flow_path = match args.get(3) {
        Some(path) => path.clone(),
        None => env::var("FLOW_CSV")
            .map_err(|_| "pass flow_1m.csv as 3rd argument or set FLOW_CSV".to_string())?,
    };

    // Число признаков схемы V2: 23 (OHLCV) + 8 (поток) = 31.
    // load_from_file отклонит модель другой схемы.
    let feature_count = FEATURE_NAMES_V1.len() + FLOW_FEATURE_COUNT;
    let model = IntradayModel::load_from_file(&args[1], feature_count)?;
    let bars = load_minute_bars_csv(&args[2])?;

    if bars.len() < MIN_REQUIRED_BARS + HORIZON_BARS {
        return Err(format!(
            "not enough bars: need at least {}, got {}",
            MIN_REQUIRED_BARS + HORIZON_BARS,
            bars.len()
        ));
    }

    let cost_rt_bps: f64 = env::var("COST_RT_BPS")
        .map_err(|_| "set COST_RT_BPS to round-trip fees + spread + slippage".to_string())?
        .parse()
        .map_err(|_| "COST_RT_BPS must be a number".to_string())?;

    if !cost_rt_bps.is_finite() || cost_rt_bps < 0.0 {
        return Err("COST_RT_BPS must be a finite non-negative number".into());
    }

    let policy = Policy {
        min_win_probability: 0.55,
        min_expected_net_return_bps: 3.0,
    };

    // Каждая строка features_all[t] рассчитана только из истории до t.
    // Будущие свечи ниже используются только для проверки результата сделки.
    let features_all = compute_features_v1_all(&bars)?;

    // Потоковые признаки: минутные агрегаты aggTrades, выровненные по свечам.
    // Окна признаков заканчиваются на свече t, будущие сделки не используются.
    let flow_map = load_flow_csv(&flow_path)?;
    // Сверка агрегатов с колонками самих свечей (покрытие и согласованность данных)
    validate_flow_against_klines(&args[2], &flow_map)?;
    let flow_aligned = align_flow_to_bars(&bars, &flow_map);
    let flow_all = compute_flow_features_all(&bars, &features_all, &flow_aligned)?;

    let first_t = MIN_REQUIRED_BARS - 1;

    // Вход моделируется на открытии t+1, а максимальный горизонт — 60 свечей.
    // Поэтому последнему сигналу нужны ещё 60 свечей для проверки исхода.
    let last_t = bars.len() - HORIZON_BARS - 1;

    let mut flat_count = 0usize;
    let mut long_count = 0usize;
    let mut short_count = 0usize;

    let mut long_pwin_pass = 0usize;
    let mut long_ev_pass = 0usize;
    let mut long_both_pass = 0usize;

    let mut short_pwin_pass = 0usize;
    let mut short_ev_pass = 0usize;
    let mut short_both_pass = 0usize;

    let mut max_long_pwin = f64::NEG_INFINITY;
    let mut max_short_pwin = f64::NEG_INFINITY;
    let mut max_long_ev = f64::NEG_INFINITY;
    let mut max_short_ev = f64::NEG_INFINITY;

    let mut win_count = 0usize;
    let mut loss_count = 0usize;
    let mut neutral_count = 0usize;

    let mut take_profit_count = 0usize;
    let mut stop_loss_count = 0usize;
    let mut timeout_count = 0usize;

    let mut sum_net_bps = 0.0;
    let mut trade_count = 0usize;
    let mut evaluated_time_count = 0usize;
    // Моменты, где признаки OHLCV есть, а потоковых нет (пропуск aggTrades): не оцениваются
    let mut skipped_no_flow_count = 0usize;
    // All hypothetical candidates, including those rejected by the EV gate.
    let mut long_scored = Vec::new();
    let mut short_scored = Vec::new();

    for t in first_t..=last_t {
        let Some(features) = features_all[t] else {
            continue;
        };
        let Some(flow_row) = flow_all[t] else {
            skipped_no_flow_count += 1;
            continue;
        };

        evaluated_time_count += 1;

        // Полный вектор признаков V2 в том же порядке, что при обучении:
        // 23 признака V1, затем 8 потоковых.
        let mut full = features.to_vec();
        full.extend_from_slice(&flow_row);

        // Считаем прогнозы явно, чтобы диагностировать каждый порог отдельно.
        let long = model.predict(&full, Side::Long)?;
        let short = model.predict(&full, Side::Short)?;

        // Simulate both sides regardless of entry gates. These rows measure
        // whether expected_net ranks outcomes; they are not executed trades.
        let long_result = evaluate_candidate(&bars, t, features, Side::Long, cost_rt_bps)?;
        let short_result = evaluate_candidate(&bars, t, features, Side::Short, cost_rt_bps)?;
        long_scored.push(ScoredCandidate {
            predicted_net_bps: long.expected_net_return_bps,
            realized_net_bps: long_result.net_return_bps,
        });
        short_scored.push(ScoredCandidate {
            predicted_net_bps: short.expected_net_return_bps,
            realized_net_bps: short_result.net_return_bps,
        });

        max_long_pwin = max_long_pwin.max(long.p_win);
        max_short_pwin = max_short_pwin.max(short.p_win);
        max_long_ev = max_long_ev.max(long.expected_net_return_bps);
        max_short_ev = max_short_ev.max(short.expected_net_return_bps);

        let long_pwin_ok = long.p_win >= policy.min_win_probability;
        let long_ev_ok = long.expected_net_return_bps >= policy.min_expected_net_return_bps;
        let long_ok = long_pwin_ok && long_ev_ok;

        let short_pwin_ok = short.p_win >= policy.min_win_probability;
        let short_ev_ok = short.expected_net_return_bps >= policy.min_expected_net_return_bps;
        let short_ok = short_pwin_ok && short_ev_ok;

        if long_pwin_ok {
            long_pwin_pass += 1;
        }
        if long_ev_ok {
            long_ev_pass += 1;
        }
        if long_ok {
            long_both_pass += 1;
        }

        if short_pwin_ok {
            short_pwin_pass += 1;
        }
        if short_ev_ok {
            short_ev_pass += 1;
        }
        if short_ok {
            short_both_pass += 1;
        }

        // Выбираем сигнал с теми же правилами, что и в основной модели.
        let signal = choose_signal(&model, &full, policy)?;

        match signal {
            Signal::Flat => {
                flat_count += 1;
            }

            Signal::Candidate { side, prediction } => {
                match side {
                    Side::Long => long_count += 1,
                    Side::Short => short_count += 1,
                }

                // Барьеры считаются по ATR из признаков V1 (как при обучении)
                let result = evaluate_candidate(&bars, t, features, side, cost_rt_bps)?;

                trade_count += 1;
                sum_net_bps += result.net_return_bps;

                match result.outcome {
                    Outcome::Win => win_count += 1,
                    Outcome::Loss => loss_count += 1,
                    Outcome::Neutral => neutral_count += 1,
                }

                match result.exit_reason {
                    ExitReason::TakeProfit => take_profit_count += 1,
                    ExitReason::StopLoss => stop_loss_count += 1,
                    ExitReason::Timeout => timeout_count += 1,
                }

                println!(
                    "t={t}, side={side:?}, \
                     P_WIN={:.3}, P_NEUTRAL={:.3}, P_LOSS={:.3}, \
                     predicted_net={:.2} bps, realized_net={:.2} bps, \
                     outcome={:?}, exit={:?}, exit_idx={}",
                    prediction.p_win,
                    prediction.p_neutral,
                    prediction.p_loss,
                    prediction.expected_net_return_bps,
                    result.net_return_bps,
                    result.outcome,
                    result.exit_reason,
                    result.exit_idx,
                );
            }
        }
    }

    // Средний net на сделку: sum(net) / число сделок
    let average_net_bps = if trade_count > 0 {
        sum_net_bps / trade_count as f64
    } else {
        0.0
    };

    println!();
    println!("=== Backtest summary ===");
    println!("evaluated decision times={evaluated_time_count}");
    println!("skipped (нет потоковых признаков)={skipped_no_flow_count}");
    println!(
        "signals: total={}, flat={}, long={}, short={}",
        flat_count + long_count + short_count,
        flat_count,
        long_count,
        short_count,
    );

    println!(
        "LONG gates: P_WIN passed={}, EV passed={}, both passed={}",
        long_pwin_pass, long_ev_pass, long_both_pass,
    );
    println!(
        "SHORT gates: P_WIN passed={}, EV passed={}, both passed={}",
        short_pwin_pass, short_ev_pass, short_both_pass,
    );

    if evaluated_time_count > 0 {
        println!(
            "maximum predictions: LONG P_WIN={:.3}, LONG expected_net={:.2} bps; \
             SHORT P_WIN={:.3}, SHORT expected_net={:.2} bps",
            max_long_pwin, max_long_ev, max_short_pwin, max_short_ev,
        );
    }

    println!(
        "executed candidate trades={}, wins={}, losses={}, neutral={}",
        trade_count, win_count, loss_count, neutral_count,
    );
    println!(
        "exits: take_profit={}, stop_loss={}, timeout={}",
        take_profit_count, stop_loss_count, timeout_count,
    );
    println!(
        "total net={:.2} bps, average net per trade={:.2} bps",
        sum_net_bps, average_net_bps,
    );

    // Ranking diagnostic over every timestamp. The overlapping candidates
    // are not a portfolio PnL simulation.
    print_expected_net_deciles("LONG", &long_scored);
    print_expected_net_deciles("SHORT", &short_scored);

    Ok(())
}

fn print_expected_net_deciles(name: &str, rows: &[ScoredCandidate]) {
    if rows.is_empty() {
        println!("{name} expected_net deciles: no rows");
        return;
    }

    let mut sorted = rows.to_vec();
    sorted.sort_by(|a, b| a.predicted_net_bps.total_cmp(&b.predicted_net_bps));
    let mut realized_means = [0.0_f64; 10];

    println!("{name} TEST by predicted expected_net decile (all candidates):");
    println!("decile | n | mean_pred_net_bps | mean_realized_net_bps | realized_positive_rate");

    for d in 0..10 {
        let start = d * sorted.len() / 10;
        let end = (d + 1) * sorted.len() / 10;
        if start == end {
            continue;
        }
        let group = &sorted[start..end];
        let mean_pred = group.iter().map(|r| r.predicted_net_bps).sum::<f64>() / group.len() as f64;
        let mean_realized =
            group.iter().map(|r| r.realized_net_bps).sum::<f64>() / group.len() as f64;
        let positive_rate =
            group.iter().filter(|r| r.realized_net_bps > 0.0).count() as f64 / group.len() as f64;
        realized_means[d] = mean_realized;
        println!(
            "{:>6} | {} | {:>17.2} | {:>20.2} | {:>21.1}%",
            d + 1,
            group.len(),
            mean_pred,
            mean_realized,
            100.0 * positive_rate
        );
    }
    println!(
        "{name} realized top-bottom spread (decile 10 - decile 1): {:.2} bps",
        realized_means[9] - realized_means[0]
    );
}

/// Моделирует одну сделку по барьерам (TP / SL / таймаут) на минутных свечах.
/// features - признаки V1 в момент решения t (нужен atr14_pct_bps для ширины барьеров).
/// Логика идентична simulate_candidate в обучающем примере, чтобы разметка при обучении
/// и проверка в backtest совпадали.
fn evaluate_candidate(
    bars: &[Kline],
    t: usize,
    features: FeaturesV1,
    side: Side,
    cost_rt_bps: f64,
) -> Result<TradeResult, String> {
    let deadline = t + HORIZON_BARS;

    if deadline >= bars.len() {
        return Err("not enough future bars to evaluate trade".into());
    }

    // Решение принимается после закрытия t.
    // Открытие t+1 — приближение цены входа для OHLCV-тестирования.
    let entry = bars[t + 1].open;
    // ATR в цене: atr_price = close * atr14_pct_bps / 10000
    let atr_price = bars[t].close * features.atr14_pct_bps / 10_000.0;

    // Расстояния барьеров: максимум из ATR-множителя и пола по издержкам
    // (cost + buffer) * entry / 10000, чтобы TP/SL не лежали внутри издержек.
    let tp_dist =
        (TAKE_PROFIT_ATR * atr_price).max((cost_rt_bps + EDGE_BUFFER_BPS) * entry / 10_000.0);
    let sl_dist =
        (STOP_LOSS_ATR * atr_price).max((cost_rt_bps + STOP_BUFFER_BPS) * entry / 10_000.0);

    let (tp_price, sl_price) = match side {
        Side::Long => (entry + tp_dist, entry - sl_dist),
        Side::Short => (entry - tp_dist, entry + sl_dist),
    };

    let mut exit_idx = deadline;
    let mut exit_price = bars[deadline].close;
    let mut exit_reason = ExitReason::Timeout;

    for i in (t + 1)..=deadline {
        let bar = bars[i];

        let (hit_tp, hit_sl) = match side {
            Side::Long => (bar.high >= tp_price, bar.low <= sl_price),
            Side::Short => (bar.low <= tp_price, bar.high >= sl_price),
        };

        // Если в одной свече задеты оба барьера, считаем,
        // что первым сработал стоп.
        if hit_sl {
            exit_idx = i;
            exit_price = match side {
                Side::Long if bar.open < sl_price => bar.open,
                Side::Short if bar.open > sl_price => bar.open,
                _ => sl_price,
            };
            exit_reason = ExitReason::StopLoss;
            break;
        }

        if hit_tp {
            exit_idx = i;
            // Не учитываем улучшение цены при гэпе за тейк-профит.
            exit_price = tp_price;
            exit_reason = ExitReason::TakeProfit;
            break;
        }
    }

    // Доходность mid-to-mid в б.п.: (exit / entry - 1) * 10000
    let raw_mid_return_bps = (exit_price / entry - 1.0) * 10_000.0;

    let side_sign = match side {
        Side::Long => 1.0,
        Side::Short => -1.0,
    };

    // Чистая доходность: gross в направлении сделки минус издержки круга
    let net_return_bps = side_sign * raw_mid_return_bps - cost_rt_bps;

    let outcome = classify_outcome(
        side,
        raw_mid_return_bps,
        net_return_bps,
        cost_rt_bps,
        EDGE_BUFFER_BPS,
    );

    Ok(TradeResult {
        net_return_bps,
        outcome,
        exit_reason,
        exit_idx,
    })
}

// /// Загружает минутные свечи Binance из CSV (12 колонок, с заголовком).
// fn load(path: &str) -> Result<Vec<Kline>, String> {
//     let file = File::open(path).map_err(|e| format!("cannot open {path}: {e}"))?;
//     let mut lines = BufReader::new(file).lines();

//     lines
//         .next()
//         .ok_or_else(|| "CSV has no header".to_string())?
//         .map_err(|e| format!("cannot read CSV header: {e}"))?;

//     let mut bars = Vec::new();

//     for (i, line) in lines.enumerate() {
//         let line = line.map_err(|e| format!("CSV read error at row {}: {e}", i + 2))?;

//         if line.trim().is_empty() {
//             continue;
//         }

//         let cols: Vec<&str> = line.trim().split(',').collect();

//         if cols.len() != 12 {
//             return Err(format!(
//                 "CSV row {}: expected 12 columns, got {}",
//                 i + 2,
//                 cols.len()
//             ));
//         }

//         let parse_f64 = |column: usize, name: &str| -> Result<f64, String> {
//             cols[column]
//                 .parse::<f64>()
//                 .map_err(|_| format!("invalid {name} at CSV row {}", i + 2))
//         };

//         let open_time_ms = cols[0]
//             .parse::<i64>()
//             .map_err(|_| format!("invalid open_time at CSV row {}", i + 2))?;

//         let close_time_ms = cols[6]
//             .parse::<i64>()
//             .map_err(|_| format!("invalid close_time at CSV row {}", i + 2))?;

//         bars.push(Kline {
//             open_time_ms,
//             open: parse_f64(1, "open")?,
//             high: parse_f64(2, "high")?,
//             low: parse_f64(3, "low")?,
//             close: parse_f64(4, "close")?,
//             base_volume: parse_f64(5, "volume")?,
//             close_time_ms,
//             quote_volume: Some(parse_f64(7, "quote_asset_volume")?),
//         });
//     }

//     Ok(bars)
// }
