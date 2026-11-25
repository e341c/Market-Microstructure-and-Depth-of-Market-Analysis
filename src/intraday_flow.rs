//! Признаки потока ордеров (order flow) из aggTrades, выровненные по минутным свечам.
//!
//! Формат aggTrades (с заголовком или без): a,p,q,f,l,T,m,M
//!   p - цена, q - количество, f/l - первый/последний id сделки в агрегате,
//!   T - время в мс (или мкс), m - покупатель является мейкером (true = агрессор ПРОДАЕТ).
//!
//! Конвейер:
//!   1. бинарник aggregate_flow: сырые сделки -> flow_1m.csv (по строке на минуту);
//!   2. load_flow_csv + align_flow_to_bars: минутные агрегаты привязываются к свечам по времени;
//!   3. validate_flow_against_klines: сверка с колонками самих свечей Binance;
//!   4. compute_flow_features_all: 8 потоковых признаков, окна заканчиваются на свече t.
//!
//! Модуль подключается в lib.rs строкой `pub mod intraday_flow;`.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader, Write};

use crate::kline::Kline;
use crate::intraday_model::FeaturesV1;

/// Длительность минуты в миллисекундах.
pub const MINUTE_MS: i64 = 60_000;

/// Порог, выше которого метка времени считается микросекундной (16 цифр), а не миллисекундной (13).
const MICROS_THRESHOLD: i64 = 100_000_000_000_000;

/// Число минут в сутках: окно «базовой» величины для сравнения.
const DAY_MINUTES: usize = 1440;

/// Среднее z верхних 10% стандартного нормального распределения:
/// E[z | z > 1.2816] = phi(1.2816) / 0.10 = 0.1755 / 0.10 = 1.755.
/// Используется для грубой оценки ожидаемой доходности верхнего дециля сигнала: |IC| * sigma_H * 1.755.
pub const TOP_DECILE_Z: f64 = 1.755;

/// Количество потоковых признаков.
pub const FLOW_FEATURE_COUNT: usize = 8;

/// Имена потоковых признаков в порядке их записи в вектор (после 23 признаков V1).
pub const FLOW_FEATURE_NAMES: [&str; FLOW_FEATURE_COUNT] = [
    "ofi_1m",                    // дисбаланс агрессивного объёма за 1 минуту, [-1, 1]
    "ofi_5m",                    // то же за 5 минут
    "ofi_15m",                   // то же за 15 минут
    "ofi_60m",                   // то же за 60 минут
    "count_imb_5m",              // дисбаланс числа сделок покупателей и продавцов за 5 минут
    "count_imb_15m",             // то же за 15 минут
    "avg_fill_ratio_15m_to_24h", // средний размер сделки за 15 мин / за сутки (активность крупных)
    "trade_vwap_dev_15m_atr",    // (close - VWAP по сделкам за 15 мин) / ATR
];

/// Агрегаты потока за одну минуту (все объёмы в котируемой валюте, т.е. сумма p*q).
#[derive(Clone, Copy, Debug, Default)]
pub struct MinuteFlow {
    /// Объём агрессивных покупок (taker buy).
    pub buy_quote: f64,
    /// Объём агрессивных продаж (taker sell).
    pub sell_quote: f64,
    /// Суммарное количество в базовой валюте (для VWAP по сделкам).
    pub base_total: f64,
    /// Число агрегированных сделок с агрессором-покупателем.
    pub n_buy: u64,
    /// Число агрегированных сделок с агрессором-продавцом.
    pub n_sell: u64,
    /// Число исходных сделок: сумма (l - f + 1) по агрегатам.
    pub fills: u64,
}

impl MinuteFlow {
    /// Добавляет одну агрегированную сделку в минутный агрегат.
    pub fn add_trade(&mut self, price: f64, qty: f64, fills: u64, buyer_is_maker: bool) {
        let quote = price * qty; // объём сделки в котируемой валюте: p * q
        if buyer_is_maker {
            // Покупатель - мейкер, значит агрессор продаёт
            self.sell_quote += quote;
            self.n_sell += 1;
        } else {
            self.buy_quote += quote;
            self.n_buy += 1;
        }
        self.base_total += qty;
        self.fills += fills;
    }
}

/// Разбирает строку aggTrades: a,p,q,f,l,T,m,M. Возвращает
/// (ключ минуты, цена, количество, число fills, buyer_is_maker) или None для заголовка и битых строк.
/// Итератор split вместо Vec: без аллокации на каждую строку.
pub fn parse_agg_line(line: &str) -> Option<(i64, f64, f64, u64, bool)> {
    let mut it = line.trim().split(',');
    let _id = it.next()?; // a - id агрегата, не нужен
    let price: f64 = it.next()?.parse().ok()?; // p; на заголовке разбор падает -> None
    let qty: f64 = it.next()?.parse().ok()?; // q
    let first: u64 = it.next()?.parse().ok()?; // f - первый id сделки в агрегате
    let last: u64 = it.next()?.parse().ok()?; // l - последний id сделки в агрегате
    let mut t: i64 = it.next()?.parse().ok()?; // T - время сделки
    if t >= MICROS_THRESHOLD {
        t /= 1000; // микросекунды -> миллисекунды
    }
    // m = true: покупатель - мейкер, значит агрессор продаёт
    let buyer_is_maker = it.next()?.trim().eq_ignore_ascii_case("true");
    // Число исходных сделок в агрегате: l - f + 1
    let fills = last.saturating_sub(first) + 1;
    // Ключ минуты = номер минуты от эпохи = floor(T / 60000)
    Some((t.div_euclid(MINUTE_MS), price, qty, fills, buyer_is_maker))
}

/// Пишет минутные агрегаты в CSV: minute_ms,buy_quote,sell_quote,base_total,n_buy,n_sell,fills.
pub fn write_flow_csv(path: &str, flow: &BTreeMap<i64, MinuteFlow>) -> Result<(), String> {
    let mut f = File::create(path).map_err(|e| format!("cannot create {path}: {e}"))?;
    writeln!(
        f,
        "minute_ms,buy_quote,sell_quote,base_total,n_buy,n_sell,fills"
    )
    .map_err(|e| e.to_string())?;
    for (minute, m) in flow {
        writeln!(
            f,
            "{},{:.8},{:.8},{:.8},{},{},{}",
            minute * MINUTE_MS,
            m.buy_quote,
            m.sell_quote,
            m.base_total,
            m.n_buy,
            m.n_sell,
            m.fills
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Читает минутные агрегаты из CSV. Ключ - номер минуты от эпохи (minute_ms / 60000).
pub fn load_flow_csv(path: &str) -> Result<HashMap<i64, MinuteFlow>, String> {
    let file = File::open(path).map_err(|e| format!("cannot open {path}: {e}"))?;
    let mut out = HashMap::new();
    for (i, line) in BufReader::new(file).lines().enumerate() {
        let line = line.map_err(|e| e.to_string())?;
        if i == 0 || line.trim().is_empty() {
            continue; // заголовок и пустые строки
        }
        let c: Vec<&str> = line.trim().split(',').collect();
        if c.len() != 7 {
            return Err(format!(
                "flow csv row {}: expected 7 columns, got {}",
                i + 1,
                c.len()
            ));
        }
        let bad = |name: &str| format!("flow csv row {}: invalid {name}", i + 1);
        let minute_ms: i64 = c[0].parse().map_err(|_| bad("minute_ms"))?;
        out.insert(
            minute_ms.div_euclid(MINUTE_MS),
            MinuteFlow {
                buy_quote: c[1].parse().map_err(|_| bad("buy_quote"))?,
                sell_quote: c[2].parse().map_err(|_| bad("sell_quote"))?,
                base_total: c[3].parse().map_err(|_| bad("base_total"))?,
                n_buy: c[4].parse().map_err(|_| bad("n_buy"))?,
                n_sell: c[5].parse().map_err(|_| bad("n_sell"))?,
                fills: c[6].parse().map_err(|_| bad("fills"))?,
            },
        );
    }
    Ok(out)
}

/// Выравнивает минутные агрегаты по свечам: для свечи i берётся агрегат минуты open_time_ms / 60000.
/// None - минуты нет в aggTrades (пропуск данных или период не покрыт).
pub fn align_flow_to_bars(
    bars: &[Kline],
    flow: &HashMap<i64, MinuteFlow>,
) -> Vec<Option<MinuteFlow>> {
    bars.iter()
        .map(|b| flow.get(&b.open_time_ms.div_euclid(MINUTE_MS)).copied())
        .collect()
}

/// Сверяет минутные агрегаты aggTrades с колонками самих свечей Binance и печатает отчёт.
/// kline_csv - файл свечей (12 колонок, с заголовком), flow - результат load_flow_csv.
/// Для каждой свечи с найденной минутой потока проверяет три равенства:
///   fills == number_of_trades                       (число исходных сделок)
///   buy_quote + sell_quote == quote_asset_volume    (общий объём в котируемой валюте)
///   buy_quote == taker_buy_quote_asset_volume       (объём агрессивных покупок)
/// Относительная погрешность: rel(a, b) = |a - b| / max(|b|, 1e-12); допуск 1e-6.
pub fn validate_flow_against_klines(
    kline_csv: &str,
    flow: &HashMap<i64, MinuteFlow>,
) -> Result<(), String> {
    let file = File::open(kline_csv).map_err(|e| format!("cannot open {kline_csv}: {e}"))?;
    // Счётчики: всего свечей, свечей с потоком, и сколько из них прошли каждое равенство
    let (mut total, mut with_flow, mut fills_eq, mut quote_ok, mut buy_ok) =
        (0usize, 0usize, 0usize, 0usize, 0usize);
    let mut first_ms = i64::MAX; // диапазон времени свечей
    let mut last_ms = i64::MIN;
    for (i, line) in BufReader::new(file).lines().enumerate() {
        let line = line.map_err(|e| e.to_string())?;
        if i == 0 || line.trim().is_empty() {
            continue; // заголовок и пустые строки
        }
        let c: Vec<&str> = line.trim().split(',').collect();
        if c.len() != 12 {
            continue;
        }
        // Колонки свечи: 0 open_time, 7 quote_asset_volume, 8 number_of_trades, 10 taker_buy_quote
        let (Ok(open_time), Ok(quote_total), Ok(n_trades), Ok(taker_buy_quote)) = (
            c[0].parse::<i64>(),
            c[7].parse::<f64>(),
            c[8].parse::<u64>(),
            c[10].parse::<f64>(),
        ) else {
            continue;
        };
        total += 1;
        first_ms = first_ms.min(open_time);
        last_ms = last_ms.max(open_time);
        let Some(m) = flow.get(&open_time.div_euclid(MINUTE_MS)) else {
            continue; // для этой минуты нет сделок в aggTrades
        };
        with_flow += 1;
        // Относительная погрешность двух чисел
        let rel = |a: f64, b: f64| (a - b).abs() / b.abs().max(1e-12);
        if m.fills == n_trades {
            fills_eq += 1;
        }
        if rel(m.buy_quote + m.sell_quote, quote_total) < 1e-6 {
            quote_ok += 1;
        }
        if rel(m.buy_quote, taker_buy_quote) < 1e-6 {
            buy_ok += 1;
        }
    }
    let pct = |a: usize, b: usize| {
        if b == 0 {
            0.0
        } else {
            100.0 * a as f64 / b as f64
        }
    };
    println!("свечей: {total}, диапазон open_time: [{first_ms}, {last_ms}] мс");
    println!(
        "свечей с потоком aggTrades: {with_flow} ({:.1}%)",
        pct(with_flow, total)
    );
    if with_flow == 0 {
        println!("ВНИМАНИЕ: нет ни одной общей минуты - периоды файлов не пересекаются.");
    } else {
        println!(
            "  fills == number_of_trades:        {:.1}%",
            pct(fills_eq, with_flow)
        );
        println!(
            "  quote == quote_asset_volume:      {:.1}%",
            pct(quote_ok, with_flow)
        );
        println!(
            "  buy_quote == taker_buy_quote:     {:.1}%",
            pct(buy_ok, with_flow)
        );
    }
    Ok(())
}

/// Префиксные суммы: p[i+1] = p[i] + values[i]. Позволяют считать сумму по окну за O(1).
fn prefix_sums(values: &[f64]) -> Vec<f64> {
    let mut p = vec![0.0; values.len() + 1];
    for i in 0..values.len() {
        p[i + 1] = p[i] + values[i];
    }
    p
}

/// Сумма значений в окне из w минут, заканчивающемся в позиции t (включительно): p[t+1] - p[t+1-w].
fn window_sum(prefix: &[f64], t: usize, w: usize) -> f64 {
    prefix[t + 1] - prefix[t + 1 - w]
}

/// Считает потоковые признаки для каждой свечи. Строка None, если нет признаков V1,
/// в последних 60 минутах есть пропуск aggTrades или знаменатель нулевой.
/// Все окна заканчиваются на свече t включительно: будущее не используется.
pub fn compute_flow_features_all(
    bars: &[Kline],
    v1: &[Option<FeaturesV1>],
    flow: &[Option<MinuteFlow>],
) -> Result<Vec<Option<[f64; FLOW_FEATURE_COUNT]>>, String> {
    let n = bars.len();
    if v1.len() != n || flow.len() != n {
        return Err("bars/features/flow length mismatch".to_string());
    }
    // Поминутные ряды (пропущенная минута = нули, но помечается в missing)
    let mut signed = Vec::with_capacity(n); // buy_quote - sell_quote
    let mut quote = Vec::with_capacity(n); // buy_quote + sell_quote
    let mut cnt_diff = Vec::with_capacity(n); // n_buy - n_sell
    let mut cnt_sum = Vec::with_capacity(n); // n_buy + n_sell
    let mut fills = Vec::with_capacity(n); // число исходных сделок
    let mut base = Vec::with_capacity(n); // количество в базовой валюте
    let mut missing = Vec::with_capacity(n); // 1.0, если минуты нет в aggTrades
    for m in flow {
        let x = m.unwrap_or_default();
        signed.push(x.buy_quote - x.sell_quote);
        quote.push(x.buy_quote + x.sell_quote);
        cnt_diff.push(x.n_buy as f64 - x.n_sell as f64);
        cnt_sum.push((x.n_buy + x.n_sell) as f64);
        fills.push(x.fills as f64);
        base.push(x.base_total);
        missing.push(if m.is_none() { 1.0 } else { 0.0 });
    }
    let (p_signed, p_quote) = (prefix_sums(&signed), prefix_sums(&quote));
    let (p_cd, p_cs) = (prefix_sums(&cnt_diff), prefix_sums(&cnt_sum));
    let (p_fills, p_base, p_miss) = (
        prefix_sums(&fills),
        prefix_sums(&base),
        prefix_sums(&missing),
    );

    // OFI окна w: sum(buy - sell) / sum(buy + sell), значения в [-1, 1]
    let ofi = |t: usize, w: usize| -> Option<f64> {
        let q = window_sum(&p_quote, t, w);
        if q > 0.0 {
            Some(window_sum(&p_signed, t, w) / q)
        } else {
            None
        }
    };
    // Дисбаланс числа сделок окна w: (n_buy - n_sell) / (n_buy + n_sell)
    let count_imb = |t: usize, w: usize| -> Option<f64> {
        let s = window_sum(&p_cs, t, w);
        if s > 0.0 {
            Some(window_sum(&p_cd, t, w) / s)
        } else {
            None
        }
    };
    // Средний размер исходной сделки в окне w: sum(quote) / sum(fills)
    let avg_fill = |t: usize, w: usize| -> Option<f64> {
        let f = window_sum(&p_fills, t, w);
        if f > 0.0 {
            Some(window_sum(&p_quote, t, w) / f)
        } else {
            None
        }
    };

    let mut out = vec![None; n];
    for t in DAY_MINUTES..n {
        let Some(v1row) = v1[t] else { continue };
        // Требуем полные данные aggTrades за последний час
        if window_sum(&p_miss, t, 60) > 0.5 {
            continue;
        }
        let (Some(o1), Some(o5), Some(o15), Some(o60), Some(c5), Some(c15), Some(af15), Some(af24)) = (
            ofi(t, 1),
            ofi(t, 5),
            ofi(t, 15),
            ofi(t, 60),
            count_imb(t, 5),
            count_imb(t, 15),
            avg_fill(t, 15),
            avg_fill(t, DAY_MINUTES),
        ) else {
            continue;
        };
        // VWAP по сделкам за 15 минут: sum(p*q) / sum(q)
        let base15 = window_sum(&p_base, t, 15);
        if base15 <= 0.0 {
            continue;
        }
        let vwap15 = window_sum(&p_quote, t, 15) / base15;
        // ATR в цене: atr_price = close * atr14_pct_bps / 10000
        let atr_price = bars[t].close * v1row.atr14_pct_bps / 10_000.0;
        if atr_price <= 0.0 || af24 <= 0.0 {
            continue;
        }
        let row = [
            o1,
            o5,
            o15,
            o60,
            c5,
            c15,
            af15 / af24, // отношение среднего размера сделки: 15 мин к суткам
            (bars[t].close - vwap15) / atr_price, // отклонение цены от VWAP сделок в единицах ATR
        ];
        if row.iter().all(|x| x.is_finite()) {
            out[t] = Some(row);
        }
    }
    Ok(out)
}
