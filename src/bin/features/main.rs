mod features;
mod plot;
use market_microstructure::file::load_minute_bars_csv;

use crate::{
    features::{FeatureConfig, create_features},
    plot::plot_features_png,
};

fn main() {
    let path = "./klines_train.csv";
    let candles = load_minute_bars_csv(path).expect("Cannot load candles from file");

    let config = FeatureConfig {
        sma1_period: 7,
        sma2_period: 25,
        rsi_period: 14,
        stochastic_k_period: 14,
        stochastic_d_period: 3,
        roc_period: 12,
        momentum_period: 10,
        volume_roc_period: 10,
    };

    let result = create_features(&candles, config).expect("cannot create features");
    let model_features = result.rows;
    let labels = result.signal;
    let positive_signals = labels
        .iter()
        .filter_map(|&signal| signal)
        .filter(|&signal| signal == 1)
        .count();
    let negative_signals = labels
        .iter()
        .filter_map(|&signal| signal)
        .filter(|&signal| signal == 0)
        .count();
    let max_sma_diff = model_features
        .iter()
        .filter_map(|row| row.sma_difference)
        .reduce(f64::max);
    let min_sma_diff = model_features
        .iter()
        .filter_map(|row| row.sma_difference)
        .reduce(f64::min);

    // let recorded_moves: Vec<_> = model_features
    //     .windows(6)
    //     .filter_map(|window| {
    //         let previous_values: Option<Vec<f64>> =
    //             window[..5].iter().map(|row| row.sma_difference).collect();

    //         let previous_values = previous_values?;
    //         let previous_avg = previous_values.iter().sum::<f64>() / 5.0;
    //         let current = window[5].sma_difference?;

    //         if previous_avg == 0.0 {
    //             return None;
    //         }

    //         let change_pct = (current - previous_avg) / previous_avg.abs() * 100.0;

    //         (change_pct >= 100.0).then_some((window[5].index, previous_avg, current, change_pct))
    //     })
    //     .collect();

    // for (index, previous, current, change_pct) in &recorded_moves {
    //     println!(
    //         "Свеча {index}: SMA difference выросла с {previous:.6} до {current:.6} ({change_pct:.2}%)"
    //     );
    // }

    #[derive(Clone, Copy)]
    enum Trend {
        Rising,
        Falling,
    }

    const REVERSAL_THRESHOLD_PCT: f64 = 100.0;

    let mut recorded_moves: Vec<(usize, f64, f64, f64)> = Vec::new();
    let mut trend: Option<Trend> = None;
    let mut candidate: Option<(usize, f64)> = None;

    for row in &model_features {
        let Some(value) = row.sma_difference else {
            continue;
        };

        let Some((candidate_index, candidate_value)) = candidate else {
            candidate = Some((row.index, value));
            continue;
        };

        match trend {
            None => {
                if value > candidate_value {
                    trend = Some(Trend::Rising);
                    candidate = Some((row.index, value));
                } else if value < candidate_value {
                    trend = Some(Trend::Falling);
                    candidate = Some((row.index, value));
                }
            }

            Some(Trend::Rising) => {
                if value >= candidate_value {
                    // Продолжается рост — обновляем возможную вершину.
                    candidate = Some((row.index, value));
                } else if candidate_value == 0.0 {
                    // Для нулевой вершины процент посчитать нельзя.
                    trend = Some(Trend::Falling);
                    candidate = Some((row.index, value));
                } else {
                    let reversal_pct = (candidate_value - value) / candidate_value.abs() * 100.0;

                    if reversal_pct >= REVERSAL_THRESHOLD_PCT {
                        // Первый индекс и третье значение задают точку экстремума
                        // для текущего plot.rs. Запись создаётся на свече подтверждения.
                        recorded_moves.push((
                            candidate_index,
                            value,
                            candidate_value,
                            reversal_pct,
                        ));

                        trend = Some(Trend::Falling);
                        candidate = Some((row.index, value));
                    }
                }
            }

            Some(Trend::Falling) => {
                if value <= candidate_value {
                    // Продолжается снижение — обновляем возможную впадину.
                    candidate = Some((row.index, value));
                } else if candidate_value == 0.0 {
                    // Для нулевой впадины процент посчитать нельзя.
                    trend = Some(Trend::Rising);
                    candidate = Some((row.index, value));
                } else {
                    let reversal_pct = (value - candidate_value) / candidate_value.abs() * 100.0;

                    if reversal_pct >= REVERSAL_THRESHOLD_PCT {
                        recorded_moves.push((
                            candidate_index,
                            value,
                            candidate_value,
                            reversal_pct,
                        ));

                        trend = Some(Trend::Rising);
                        candidate = Some((row.index, value));
                    }
                }
            }
        }
    }

    println!("sma difference 100% move {}", recorded_moves.len());

    println!(
        "свечей: {}, строк признаков: {}, сигналов: {}",
        candles.len(),
        model_features.len(),
        labels.iter().flatten().count(),
    );
    println!("Положительных сигналов: {positive_signals}");
    println!("Отрицательных сигналов: {negative_signals}");
    println!("Максимум SMA difference: {max_sma_diff:?}");
    println!("Минимум SMA difference: {min_sma_diff:?}");

    let _ = plot_features_png(
        "./plot.png",
        &model_features,
        &labels,
        900,
        1100,
        &recorded_moves,
    );
}
