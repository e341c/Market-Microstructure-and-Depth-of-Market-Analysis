use std::fmt::Write as _;

use super::{ExcursionStats, Polarity, Signal, calculate_mfe_mae, validity_from};

struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

fn flip(p: Polarity) -> Polarity {
    match p {
        Polarity::Positive => Polarity::Negative,
        Polarity::Negative => Polarity::Positive,
        Polarity::Flat => Polarity::Flat,
    }
}

/// (n, target first %, stop first %, gross expectancy USD, средняя цена входа)
fn bucket_summary(
    signals: &[Signal],
    lo: f64,
    hi: f64,
    target: f64,
    stop: f64,
) -> Option<(usize, f64, f64, f64, f64)> {
    let stats: Vec<&ExcursionStats> = signals
        .iter()
        .filter(|s| s.weight >= lo && s.weight < hi)
        .filter_map(|s| s.stats.as_ref())
        .collect();
    let n = stats.len();
    if n == 0 {
        return None;
    }

    let (mut tf, mut sf) = (0usize, 0usize);
    let mut realized = 0.0_f64;
    for s in &stats {
        match (s.first_target_after_us, s.first_stop_after_us) {
            (Some(t), Some(st)) if t < st => {
                tf += 1;
                realized += s.first_target_return.unwrap_or(target);
            }
            (Some(_), None) => {
                tf += 1;
                realized += s.first_target_return.unwrap_or(target);
            }
            (_, Some(_)) => {
                sf += 1;
                realized += s.first_stop_return.unwrap_or(-stop);
            }
            (None, None) => realized += s.final_return,
        }
    }
    let nf = n as f64;
    let expectancy = realized / nf;

    let mean_entry = stats.iter().map(|s| s.entry_price).sum::<f64>() / nf;
    Some((
        n,
        tf as f64 * 100.0 / nf,
        sf as f64 * 100.0 / nf,
        expectancy,
        mean_entry,
    ))
}

const N_SEEDS: u64 = 20;

fn random_from(signals: &[Signal], flipped: &[Signal], seed: u64) -> Vec<Signal> {
    let mut rng = XorShift(0x9E37_79B9_7F4A_7C15 ^ seed.wrapping_mul(0xD1B5_4A32_D192_ED03));
    for _ in 0..8 {
        rng.next(); // прогрев: близкие seed дают похожие первые значения
    }
    signals
        .iter()
        .zip(flipped)
        .map(|(r, f)| {
            if rng.next() & 1 == 0 {
                r.clone()
            } else {
                f.clone()
            }
        })
        .collect()
}

fn mean_std(v: &[f64]) -> (f64, f64) {
    if v.is_empty() {
        return (0.0, 0.0);
    }
    let m = v.iter().sum::<f64>() / v.len() as f64;
    let var = v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / v.len() as f64;
    (m, var.sqrt())
}

pub fn print_control(
    out: &mut String,
    signals: &[Signal],
    data: &[Vec<f64>],
    ts: &[i64],
    target: f64,
    stop: f64,
) {
    let buckets = [(0.5, 0.7), (0.7, 0.9), (0.9, 1.01)];

    // Те же строки и weight, но другая polarity.
    let flipped: Vec<Signal> = signals
        .iter()
        .map(|s| {
            let mut c = s.clone();
            c.polarity = flip(s.polarity);
            c.stats = calculate_mfe_mae(&c, data, ts, target, stop);
            c.validity = validity_from(&c.stats);
            c
        })
        .collect();

    // Случайная polarity по N_SEEDS: считаем по одному seed за раз, чтобы не держать всё в памяти.
    let mut rand_gross: Vec<Vec<f64>> = vec![Vec::new(); buckets.len()];
    for seed in 1..=N_SEEDS {
        let random = random_from(signals, &flipped, seed);
        for (bi, (lo, hi)) in buckets.iter().enumerate() {
            if let Some(v) = bucket_summary(&random, *lo, *hi, target, stop) {
                rand_gross[bi].push(v.3);
            }
        }
    }

    writeln!(
        out,
        "\n========== CONTROL: real vs flipped vs random (target {} / stop {}) ==========",
        target, stop
    )
    .unwrap();

    for (bi, (lo, hi)) in buckets.iter().enumerate() {
        let (Some(r), Some(f)) = (
            bucket_summary(signals, *lo, *hi, target, stop),
            bucket_summary(&flipped, *lo, *hi, target, stop),
        ) else {
            continue;
        };
        let (rm, rsd) = mean_std(&rand_gross[bi]);

        writeln!(out, "\n--- weight [{}, {}) : {} signals ---", lo, hi, r.0).unwrap();
        for (name, v) in [("real", r), ("flipped", f)] {
            writeln!(
                out,
                "{:<8} target first {:>6.2}%  stop first {:>6.2}%  gross {:>8.4} USD",
                name, v.1, v.2, v.3
            )
            .unwrap();
        }
        writeln!(
            out,
            "random   mean {:>8.4}  std {:.4} USD  ({} seeds)",
            rm, rsd, N_SEEDS
        )
        .unwrap();

        // real = +edge + common, flipped = -edge + common
        let edge = (r.3 - f.3) / 2.0;
        let common = (r.3 + f.3) / 2.0;
        writeln!(
            out,
            "Edge   (real - flipped)/2: {:.4} USD = {:.4} bps",
            edge,
            edge / r.4 * 10_000.0
        )
        .unwrap();
        writeln!(
            out,
            "Common (real + flipped)/2: {:.4} USD  (не зависит от направления)",
            common
        )
        .unwrap();
    }
}
