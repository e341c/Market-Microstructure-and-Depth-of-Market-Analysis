// ---- Модель доходности: ridge + walk-forward, baseline = 0 ----

use market_microstructure::fetchdata::Kline;

const N: usize = 4;
const LOOKBACK: usize = 20;
const TRAIN_WINDOW: usize = 288; // ~1 сутки 5-минуток
const ROUND_TRIP_FEE_BPS: f64 = 10.0; // подставь свою комиссию за круг

struct Bar {
    high: f64,
    low: f64,
    close: f64,
    volume: f64,
}

fn parse_bars(klines: &[Kline]) -> Option<Vec<Bar>> {
    klines
        .iter()
        .map(|k| {
            Some(Bar {
                high: k.high.parse().ok()?,
                low: k.low.parse().ok()?,
                close: k.close.parse().ok()?,
                volume: k.volume.parse().ok()?,
            })
        })
        .collect()
}

fn features(bars: &[Bar], t: usize) -> Option<[f64; N]> {
    if t < LOOKBACK {
        return None;
    }
    let b = &bars[t];
    let vol_mean = bars[t - LOOKBACK..t].iter().map(|x| x.volume).sum::<f64>() / LOOKBACK as f64;

    if b.close <= 0.0
        || b.low <= 0.0
        || b.volume <= 0.0
        || vol_mean <= 0.0
        || bars[t - 1].close <= 0.0
        || bars[t - 3].close <= 0.0
    {
        return None;
    }

    Some([
        (b.close / bars[t - 1].close).ln(),
        (b.close / bars[t - 3].close).ln(),
        (b.high / b.low).ln(),
        (b.volume / vol_mean).ln(),
    ])
}

struct Ridge {
    mean: [f64; N],
    std: [f64; N],
    y_mean: f64,
    w: [f64; N],
}

impl Ridge {
    fn fit(xs: &[[f64; N]], ys: &[f64], lambda: f64) -> Option<Ridge> {
        let n = xs.len();
        if n < N + 2 {
            return None;
        }

        let mut mean = [0.0; N];
        let mut std = [0.0; N];
        for k in 0..N {
            mean[k] = xs.iter().map(|x| x[k]).sum::<f64>() / n as f64;
            let var = xs.iter().map(|x| (x[k] - mean[k]).powi(2)).sum::<f64>() / n as f64;
            std[k] = if var.sqrt() < 1e-12 { 1.0 } else { var.sqrt() };
        }
        let y_mean = ys.iter().sum::<f64>() / n as f64;

        // A = ZᵀZ + λI,  b = Zᵀ(y − ȳ)
        let mut a = [[0.0; N]; N];
        let mut b = [0.0; N];
        for (x, y) in xs.iter().zip(ys) {
            let mut z = [0.0; N];
            for k in 0..N {
                z[k] = (x[k] - mean[k]) / std[k];
            }
            for i in 0..N {
                b[i] += z[i] * (y - y_mean);
                for j in 0..N {
                    a[i][j] += z[i] * z[j];
                }
            }
        }
        for i in 0..N {
            a[i][i] += lambda;
        }

        let w = solve(a, b)?;
        Some(Ridge {
            mean,
            std,
            y_mean,
            w,
        })
    }

    fn predict(&self, x: &[f64; N]) -> f64 {
        let mut p = self.y_mean;
        for k in 0..N {
            p += self.w[k] * (x[k] - self.mean[k]) / self.std[k];
        }
        p
    }
}

// Гаусс с выбором главного элемента.
fn solve(mut a: [[f64; N]; N], mut b: [f64; N]) -> Option<[f64; N]> {
    for col in 0..N {
        let pivot = (col..N).max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs()))?;
        if a[pivot][col].abs() < 1e-12 {
            return None;
        }
        a.swap(col, pivot);
        b.swap(col, pivot);

        for row in col + 1..N {
            let f = a[row][col] / a[col][col];
            for k in col..N {
                a[row][k] -= f * a[col][k];
            }
            b[row] -= f * b[col];
        }
    }

    let mut x = [0.0; N];
    for i in (0..N).rev() {
        let s: f64 = (i + 1..N).map(|j| a[i][j] * x[j]).sum();
        x[i] = (b[i] - s) / a[i][i];
    }
    Some(x)
}

pub fn evaluate_return_model(klines: &[Kline], interval_minutes: usize) {
    let Some(bars) = parse_bars(klines) else {
        println!("Не удалось разобрать свечи");
        return;
    };
    let feats: Vec<Option<[f64; N]>> = (0..bars.len()).map(|t| features(&bars, t)).collect();

    println!("\n=== Доходность: ridge walk-forward, baseline = 0 ===");

    for horizon_minutes in [15, 30, 60] {
        if horizon_minutes % interval_minutes != 0 {
            continue;
        }
        let h = horizon_minutes / interval_minutes;
        let target = |j: usize| (bars[j + h].close / bars[j].close).ln();

        let start = LOOKBACK + h + TRAIN_WINDOW;
        let end = bars.len().saturating_sub(h);

        for lambda in [10.0, 100.0, 1000.0] {
            let mut n = 0usize;
            let mut sse_model = 0.0;
            let mut sse_zero = 0.0;
            let mut hits = 0usize;
            let mut nonzero = 0usize;
            let mut net_sum_bps = 0.0;
            let mut strong_n = 0usize;
            let mut strong_net_bps = 0.0;

            // Тестовые точки идут с шагом h, чтобы метки не перекрывались.
            for t in (start..end).step_by(h) {
                let Some(x_t) = feats[t] else { continue };

                // Обучаемся только на точках, чья метка известна к моменту t: j + h <= t.
                let mut xs = Vec::with_capacity(TRAIN_WINDOW);
                let mut ys = Vec::with_capacity(TRAIN_WINDOW);
                for j in (t + 1 - h - TRAIN_WINDOW)..=(t - h) {
                    if let Some(x) = feats[j] {
                        xs.push(x);
                        ys.push(target(j));
                    }
                }

                let Some(model) = Ridge::fit(&xs, &ys, lambda) else {
                    continue;
                };

                let pred = model.predict(&x_t);
                let y = target(t);

                n += 1;
                sse_model += (y - pred).powi(2);
                sse_zero += y * y;

                if y != 0.0 {
                    nonzero += 1;
                    if pred.signum() == y.signum() {
                        hits += 1;
                    }
                }

                let net_bps = pred.signum() * y * 10_000.0 - ROUND_TRIP_FEE_BPS;
                net_sum_bps += net_bps;

                // Торгуем только если ожидаемое движение перекрывает комиссию.
                if pred.abs() * 10_000.0 > ROUND_TRIP_FEE_BPS {
                    strong_n += 1;
                    strong_net_bps += net_bps;
                }
            }

            if n == 0 || sse_zero == 0.0 {
                println!("{horizon_minutes}m λ={lambda}: недостаточно данных");
                continue;
            }

            let r2 = 1.0 - sse_model / sse_zero;
            let hit = hits as f64 / nonzero.max(1) as f64 * 100.0;
            let hit_se = 50.0 / (nonzero.max(1) as f64).sqrt(); // ≈ SE при p=0.5, в п.п.

            println!(
                "{horizon_minutes}m λ={lambda:>6}: n={n}, OOS R²={:.3}%, hit={hit:.1}% (±{:.1}), net/сделку={:.2} б.п., сделок с |ŷ|>fee: {strong_n} ({:.2} б.п.)",
                r2 * 100.0,
                hit_se * 2.0,
                net_sum_bps / n as f64,
                if strong_n > 0 {
                    strong_net_bps / strong_n as f64
                } else {
                    0.0
                },
            );
        }
    }
}
