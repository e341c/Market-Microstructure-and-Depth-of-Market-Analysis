mod log_parser;
use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufWriter, Write};
mod random;

// Потенциально сильный сигнал вверх:
// - OBI положительный и растёт.
// - OBI 1 sec начинает расти.
// - OFI 500 ms положительный.
// - Microprice движется выше midprice.

// Потенциально сильный сигнал вниз:
// - OBI снижается.
// - OBI 1 sec начинает снижаться.
// - OFI 500 ms отрицательный.
// - Microprice приближается к midprice или опускается ниже него.

// Модель исполнения (едина для Valid/Invalid и MFE/MAE):
// - Long:  вход по Ask, выход по Bid.
// - Short: вход по Bid, выход по Ask.
//
// MFE (Maximum Favorable Excursion) — максимальное движение в нашу пользу после входа.
// MAE (Maximum Adverse Excursion)   — максимальное движение против нашей позиции после входа.

// Комиссия в б.п. на одну сторону (вход или выход). 10 б.п. = 0.1%.
// Отрицательное значение = ребейт.
const FEE_BPS_PER_SIDE: [f64; 5] = [10.0, 5.0, 2.0, 0.0, -1.0];

#[derive(Debug, Clone, Copy)]
struct ExcursionStats {
    entry_price: f64,
    entry_ts_us: i64,

    mfe: f64,
    mfe_index: usize,
    mfe_after_us: i64,

    mae: f64,
    mae_index: usize,
    mae_after_us: i64,

    final_return: f64,
    final_index: usize,

    first_target_return: Option<f64>,
    first_stop_return: Option<f64>,

    first_target_index: Option<usize>,
    first_stop_index: Option<usize>,
    // Время от входа до первого достижения цели / стопа.
    first_target_after_us: Option<i64>,
    first_stop_after_us: Option<i64>,
}

#[derive(Debug, Clone)]
struct Signal {
    index: usize,
    weight: f64,
    polarity: Polarity,
    validity: Validity,
    timeout_us: i64,
    delay_us: i64,
    stats: Option<ExcursionStats>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Validity {
    Valid,
    Invalid,
    Unverified,
    Expired,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Polarity {
    Positive,
    Negative,
    Flat,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum ExcursionOutcome {
    TargetOnly,
    StopOnly,
    TargetThenStop,
    StopThenTarget,
    Neither,
}

fn classify_excursion(s: &ExcursionStats) -> ExcursionOutcome {
    match (s.first_target_index, s.first_stop_index) {
        (Some(_), None) => ExcursionOutcome::TargetOnly,
        (None, Some(_)) => ExcursionOutcome::StopOnly,
        (Some(t), Some(st)) if t < st => ExcursionOutcome::TargetThenStop,
        (Some(_), Some(_)) => ExcursionOutcome::StopThenTarget,
        (None, None) => ExcursionOutcome::Neither,
    }
}

fn validity_from(stats: &Option<ExcursionStats>) -> Validity {
    match stats.as_ref().map(classify_excursion) {
        None => Validity::Unverified,
        Some(ExcursionOutcome::TargetOnly | ExcursionOutcome::TargetThenStop) => Validity::Valid,
        Some(ExcursionOutcome::StopOnly | ExcursionOutcome::StopThenTarget) => Validity::Invalid,
        Some(ExcursionOutcome::Neither) => Validity::Expired,
    }
}

#[derive(Debug, Default)]
struct ExcursionSummary {
    count: usize,

    total_mfe: f64,
    total_mae: f64,

    max_mfe: f64,
    max_mae: f64,

    mfe_values: Vec<f64>,
    mae_values: Vec<f64>,

    target_only: usize,
    stop_only: usize,
    target_then_stop: usize,
    stop_then_target: usize,
    neither: usize,
}

impl ExcursionSummary {
    fn add(&mut self, stats: &ExcursionStats) {
        self.count += 1;

        self.total_mfe += stats.mfe;
        self.total_mae += stats.mae;

        self.max_mfe = self.max_mfe.max(stats.mfe);
        self.max_mae = self.max_mae.max(stats.mae);

        self.mfe_values.push(stats.mfe);
        self.mae_values.push(stats.mae);

        match classify_excursion(stats) {
            ExcursionOutcome::TargetOnly => self.target_only += 1,
            ExcursionOutcome::StopOnly => self.stop_only += 1,
            ExcursionOutcome::TargetThenStop => self.target_then_stop += 1,
            ExcursionOutcome::StopThenTarget => self.stop_then_target += 1,
            ExcursionOutcome::Neither => self.neither += 1,
        }
    }

    fn average_mfe(&self) -> f64 {
        if self.count == 0 {
            return 0.0;
        }
        self.total_mfe / self.count as f64
    }

    fn average_mae(&self) -> f64 {
        if self.count == 0 {
            return 0.0;
        }
        self.total_mae / self.count as f64
    }

    fn median(values: &mut [f64]) -> f64 {
        if values.is_empty() {
            return 0.0;
        }

        values.sort_by(|a, b| a.total_cmp(b));

        let mid = values.len() / 2;

        if values.len() % 2 == 0 {
            (values[mid - 1] + values[mid]) / 2.0
        } else {
            values[mid]
        }
    }

    fn median_mfe(&mut self) -> f64 {
        Self::median(&mut self.mfe_values)
    }

    fn median_mae(&mut self) -> f64 {
        Self::median(&mut self.mae_values)
    }
}

pub fn analyse(n: &str, target_move: f64, stop_move: f64, timeout_ms: u64, entry_delay_ms: i64) {
    let log = log_parser::read_log_file(n).expect("[ERROR] parse log file failed");
    let data = &log.data;
    let ts = &log.ts_us;
    let timeout_us = timeout_ms as i64 * 1000;

    let mut signals: Vec<Signal> = Vec::new();

    for i in 1..data.len() {
        let (polarity, weight) = calculate_signal(&data[i], &data[i - 1]);

        if polarity == Polarity::Flat {
            continue;
        }

        let mut signal = Signal {
            index: i,
            weight,
            polarity,
            timeout_us,
            delay_us: entry_delay_ms * 1000,
            validity: Validity::Unverified,
            stats: None,
        };

        signal.stats = calculate_mfe_mae(&signal, data, ts, target_move, stop_move);
        signal.validity = validity_from(&signal.stats);

        signals.push(signal);
    }

    let count = |polarity: Polarity, validity: Validity| {
        signals
            .iter()
            .filter(|s| s.polarity == polarity && s.validity == validity)
            .count()
    };

    let positive_valid = count(Polarity::Positive, Validity::Valid);
    let positive_invalid = count(Polarity::Positive, Validity::Invalid);
    let negative_valid = count(Polarity::Negative, Validity::Valid);
    let negative_invalid = count(Polarity::Negative, Validity::Invalid);

    let expired = signals
        .iter()
        .filter(|s| s.validity == Validity::Expired)
        .count();

    let failed_mfe_mae = signals.iter().filter(|s| s.stats.is_none()).count();

    let mut positive_summary = ExcursionSummary::default();
    let mut negative_summary = ExcursionSummary::default();

    for signal in &signals {
        let Some(stats) = &signal.stats else {
            continue;
        };

        match signal.polarity {
            Polarity::Positive => positive_summary.add(stats),
            Polarity::Negative => negative_summary.add(stats),
            Polarity::Flat => {}
        }
    }

    let mut out = String::new();

    writeln!(out, "\n========== SIGNAL ANALYSIS ==========").unwrap();
    writeln!(out, "Target:           {}", target_move).unwrap();
    writeln!(out, "Stop:             {}", stop_move).unwrap();
    writeln!(out, "Timeout:          {}", timeout_ms).unwrap();
    writeln!(out, "Delay:            {}", entry_delay_ms).unwrap();
    writeln!(out, "Positive valid:   {}", positive_valid).unwrap();
    writeln!(out, "Positive invalid: {}", positive_invalid).unwrap();
    writeln!(out, "Negative valid:   {}", negative_valid).unwrap();
    writeln!(out, "Negative invalid: {}", negative_invalid).unwrap();
    writeln!(out, "Expired:          {}", expired).unwrap();
    writeln!(out, "MFE/MAE calculation failed: {}", failed_mfe_mae).unwrap();

    print_excursion_summary(&mut out, "POSITIVE", &mut positive_summary);
    print_excursion_summary(&mut out, "NEGATIVE", &mut negative_summary);
    print_weight_buckets(&mut out, &signals, target_move, stop_move);

    random::print_control(&mut out, &signals, data, ts, target_move, stop_move);

    let suffix = format!(
        "t{}_s{}_to{}_d{}",
        target_move, stop_move, timeout_ms, entry_delay_ms
    );

    let csv_path = format!("signals_{}.csv", suffix);
    export_signals_csv(&csv_path, &signals).expect("[ERROR] csv export failed");
    writeln!(out, "\nCSV saved: {}", csv_path).unwrap();

    let report_path = format!("report_{}.txt", suffix);
    std::fs::write(&report_path, &out).expect("[ERROR] report write failed");

    print!("{}", out);
    println!("Report saved: {}", report_path);
}

fn print_excursion_summary(out: &mut String, name: &str, summary: &mut ExcursionSummary) {
    writeln!(out, "\n========== {} MFE / MAE ==========", name).unwrap();

    writeln!(out, "Signals:            {}", summary.count).unwrap();

    writeln!(out, "Average MFE:        {:.4}", summary.average_mfe()).unwrap();
    writeln!(out, "Average MAE:        {:.4}", summary.average_mae()).unwrap();

    writeln!(out, "Median MFE:         {:.4}", summary.median_mfe()).unwrap();
    writeln!(out, "Median MAE:         {:.4}", summary.median_mae()).unwrap();

    writeln!(out, "Max MFE:            {:.4}", summary.max_mfe).unwrap();
    writeln!(out, "Max MAE:            {:.4}", summary.max_mae).unwrap();

    writeln!(out, "Target only:        {}", summary.target_only).unwrap();
    writeln!(out, "Stop only:          {}", summary.stop_only).unwrap();
    writeln!(out, "Target then stop:   {}", summary.target_then_stop).unwrap();
    writeln!(out, "Stop then target:   {}", summary.stop_then_target).unwrap();
    writeln!(out, "Neither reached:    {}", summary.neither).unwrap();
}

// Ответы на вопросы 1-3: доля сигналов по порогам MFE / MAE в разрезе weight.
fn print_weight_buckets(out: &mut String, signals: &[Signal], target: f64, stop: f64) {
    let buckets = [(0.5, 0.7), (0.7, 0.9), (0.9, 1.01)];
    let mfe_thresholds = [5.0, 10.0, 15.0, 20.0];
    let mae_thresholds = [2.0, 5.0, 10.0];

    writeln!(out, "\n========== MFE / MAE BY WEIGHT ==========").unwrap();

    for (lo, hi) in buckets {
        let stats: Vec<&ExcursionStats> = signals
            .iter()
            .filter(|s| s.weight >= lo && s.weight < hi)
            .filter_map(|s| s.stats.as_ref())
            .collect();

        let n = stats.len();
        if n == 0 {
            continue;
        }
        let pct = |c: usize| c as f64 * 100.0 / n as f64;

        writeln!(out, "\n--- weight [{}, {}) : {} signals ---", lo, hi, n).unwrap();

        for t in mfe_thresholds {
            let c = stats.iter().filter(|s| s.mfe > t).count();
            writeln!(out, "MFE > {:>4}: {:>6.2}%", t, pct(c)).unwrap();
        }
        for t in mae_thresholds {
            let c = stats.iter().filter(|s| s.mae < t).count();
            writeln!(out, "MAE < {:>4}: {:>6.2}%", t, pct(c)).unwrap();
        }

        let mut target_first = 0usize;
        let mut stop_first = 0usize;
        let mut neither = 0usize;
        let mut realized = 0.0_f64; // FIX: внутри бакета, обнуляется для каждого
        let mut t_times: Vec<f64> = Vec::new();
        let mut s_times: Vec<f64> = Vec::new();

        for s in &stats {
            match (s.first_target_after_us, s.first_stop_after_us) {
                (Some(t), Some(st)) if t < st => {
                    target_first += 1;
                    t_times.push(t as f64 / 1000.0);
                    realized += s.first_target_return.unwrap_or(target); // FIX
                }
                (Some(t), None) => {
                    target_first += 1;
                    t_times.push(t as f64 / 1000.0);
                    realized += s.first_target_return.unwrap_or(target); // FIX
                }
                (_, Some(st)) => {
                    stop_first += 1;
                    s_times.push(st as f64 / 1000.0);
                    realized += s.first_stop_return.unwrap_or(-stop); // FIX
                }
                (None, None) => {
                    neither += 1;
                    realized += s.final_return; // FIX
                }
            }
        }

        let expectancy = realized / n as f64;

        writeln!(out, "-- order-aware (target {} / stop {}) --", target, stop).unwrap();
        writeln!(out, "Target first: {:>6.2}%", pct(target_first)).unwrap();
        writeln!(out, "Stop first:   {:>6.2}%", pct(stop_first)).unwrap();
        writeln!(out, "Neither:      {:>6.2}%", pct(neither)).unwrap();
        writeln!(
            out,
            "median ms to target {:.1} / median ms to stop {:.1}",
            ExcursionSummary::median(&mut t_times),
            ExcursionSummary::median(&mut s_times)
        )
        .unwrap();

        let mean_entry = stats.iter().map(|s| s.entry_price).sum::<f64>() / n as f64;
        let usd_per_bps_round_trip = 2.0 * mean_entry / 10_000.0;

        writeln!(out, "Expectancy gross:       {:.4} USD", expectancy).unwrap();
        writeln!(
            out,
            "Expectancy gross:       {:.4} bps of price",
            expectancy / mean_entry * 10_000.0
        )
        .unwrap();
        writeln!(
            out,
            "Break-even fee:         {:.4} bps/side",
            expectancy / usd_per_bps_round_trip
        )
        .unwrap();
        for fee in FEE_BPS_PER_SIDE {
            writeln!(
                out,
                "Net expectancy @ {:>5.1} bps/side: {:>10.4} USD",
                fee,
                expectancy - fee * usd_per_bps_round_trip
            )
            .unwrap();
        }

        let mut mfe_values: Vec<f64> = stats.iter().map(|s| s.mfe).collect();
        let mut mae_values: Vec<f64> = stats.iter().map(|s| s.mae).collect();
        writeln!(
            out,
            "median MFE {:.3} / median MAE {:.3}",
            ExcursionSummary::median(&mut mfe_values),
            ExcursionSummary::median(&mut mae_values)
        )
        .unwrap();
    }
}

fn export_signals_csv(path: &str, signals: &[Signal]) -> std::io::Result<()> {
    let mut w = BufWriter::new(File::create(path)?);

    writeln!(
        w,
        "signal_index,entry_ts_us,polarity,weight,entry_price,mfe,mfe_index,mfe_after_ms,\
         mae,mae_index,mae_after_ms,final_return,final_index,validity,\
         first_target_index,first_target_after_ms,first_stop_index,first_stop_after_ms"
    )?;

    let opt = |v: Option<usize>| v.map(|x| x.to_string()).unwrap_or_default();
    let opt_ms = |v: Option<i64>| {
        v.map(|x| format!("{:.3}", x as f64 / 1000.0))
            .unwrap_or_default()
    };

    for s in signals {
        let Some(st) = &s.stats else {
            continue;
        };

        writeln!(
            w,
            "{},{},{:?},{:.4},{},{},{},{:.3},{},{},{:.3},{},{},{:?},{},{},{},{}",
            s.index,
            st.entry_ts_us,
            s.polarity,
            s.weight,
            st.entry_price,
            st.mfe,
            st.mfe_index,
            st.mfe_after_us as f64 / 1000.0,
            st.mae,
            st.mae_index,
            st.mae_after_us as f64 / 1000.0,
            st.final_return,
            st.final_index,
            s.validity,
            opt(st.first_target_index),
            opt_ms(st.first_target_after_us),
            opt(st.first_stop_index),
            opt_ms(st.first_stop_after_us),
        )?;
    }

    w.flush()
}

fn sign(a: f64, b: f64) -> i32 {
    if a > b {
        1
    } else if a < b {
        -1
    } else {
        0
    }
}

fn calculate_signal(current: &[f64], previous: &[f64]) -> (Polarity, f64) {
    let pa = current[0];
    let pb = current[1];
    let microprice = current[2];
    let obi = current[3];
    let obi1s = current[4];
    let ofi500 = current[6];

    let prev_obi = previous[3];
    let prev_obi1s = previous[4];

    let midprice = (pa + pb) / 2.0;

    let obi_momentum = sign(obi, prev_obi);
    let obi1s_momentum = sign(obi1s, prev_obi1s);
    let ofi_direction = sign(ofi500, 0.0);
    let microprice_direction = sign(microprice, midprice);

    // Веса в целых процентах, чтобы порог 50 не зависел от погрешности f64.
    let score: i32 =
        obi_momentum * 20 + obi1s_momentum * 20 + ofi_direction * 35 + microprice_direction * 25;

    let polarity = if score >= 50 {
        Polarity::Positive
    } else if score <= -50 {
        Polarity::Negative
    } else {
        Polarity::Flat
    };

    (polarity, score.abs() as f64 / 100.0)
}

fn calculate_mfe_mae(
    signal: &Signal,
    data: &[Vec<f64>],
    ts: &[i64],
    target: f64,
    stop: f64,
) -> Option<ExcursionStats> {
    // Вход на первой строке не раньше времени сигнала + задержка.
    let sig_ts = ts[signal.index];
    let entry_idx = ts
        .partition_point(|&t| t < sig_ts + signal.delay_us)
        .max(signal.index);

    let entry_row = data.get(entry_idx)?;
    if entry_row.len() < 2 {
        return None;
    }

    let entry_price = match signal.polarity {
        Polarity::Positive => entry_row[0], // Long: вход по Ask
        Polarity::Negative => entry_row[1], // Short: вход по Bid
        Polarity::Flat => return None,
    };

    let entry_ts = ts[entry_idx];
    let deadline = entry_ts + signal.timeout_us;
    let start = entry_idx + 1;
    // ts отсортирован, поэтому конец окна ищем бинарным поиском.
    let end = ts.partition_point(|&t| t <= deadline);

    if start >= end {
        return None;
    }

    let mut mfe = 0.0_f64;
    let mut mae = 0.0_f64;
    let (mut mfe_index, mut mae_index) = (start, start);
    let (mut mfe_after, mut mae_after) = (0_i64, 0_i64);

    let mut final_return = 0.0;
    let mut final_index = start;

    let mut first_target: Option<(usize, i64)> = None;
    let mut first_stop: Option<(usize, i64)> = None;

    let mut target_ret: Option<f64> = None;
    let mut stop_ret: Option<f64> = None;

    for i in start..end {
        let row = &data[i];
        if row.len() < 2 {
            continue;
        }

        let ask = row[0];
        let bid = row[1];
        let after = ts[i] - entry_ts;

        let current_return = match signal.polarity {
            Polarity::Positive => bid - entry_price,
            Polarity::Negative => entry_price - ask,
            Polarity::Flat => return None,
        };

        if current_return > mfe {
            mfe = current_return;
            mfe_index = i;
            mfe_after = after;
        }
        if -current_return > mae {
            mae = -current_return;
            mae_index = i;
            mae_after = after;
        }

        if first_target.is_none() && current_return >= target {
            first_target = Some((i, after));
            target_ret = Some(current_return);
        }
        if first_stop.is_none() && -current_return >= stop {
            first_stop = Some((i, after));
            stop_ret = Some(current_return);
        }

        final_return = current_return;
        final_index = i;
    }

    Some(ExcursionStats {
        entry_price,
        entry_ts_us: entry_ts,
        mfe,
        mfe_index,
        mfe_after_us: mfe_after,
        mae,
        mae_index,
        mae_after_us: mae_after,
        final_return,
        final_index,
        first_target_return: target_ret,
        first_stop_return: stop_ret,
        first_target_index: first_target.map(|x| x.0),
        first_stop_index: first_stop.map(|x| x.0),
        first_target_after_us: first_target.map(|x| x.1),
        first_stop_after_us: first_stop.map(|x| x.1),
    })
}

fn main() {
    let metrics_file_path: &str = "./metrics.log";
    let target_move_arr: Vec<f64> = vec![50.0, 100.0];
    let stop_move_arr: Vec<f64> = vec![50.0];
    let timeout_arr: Vec<u64> = vec![5000, 30000];
    let delay_arr: Vec<i64> = vec![50, 100, 200, 300];
    let mut metrics_matrix: Vec<(Vec<f64>, Vec<f64>, Vec<u64>, Vec<i64>)> = Vec::new();

    metrics_matrix.push((target_move_arr, stop_move_arr, timeout_arr, delay_arr));

    for (targets, stops, timeouts, delays) in &metrics_matrix {
        for target in targets {
            for stop in stops {
                for timeout in timeouts {
                    for delay in delays {
                        let metrics = analyse(metrics_file_path, *target, *stop, *timeout, *delay);
                    }
                }
            }
        }
    }
}
