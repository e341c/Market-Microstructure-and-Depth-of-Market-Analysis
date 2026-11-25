mod features;
mod plot;
use market_microstructure::file::load_minute_bars_csv;

use crate::{
    features::{FeatureConfig, create_features},
    plot::plot_features_png,
};

// TODO:
// 1. Test new signals
// 2. Create trading strategy based on this new signals
// 3. Add for trading strategy mechanisms for money securing, for example if volume momentom begin to be so high and we still have an open postion
// 4. Paper trading strategy
// 5. ML? For what purpose it can be used here? Maybe for better signal finding?
// 6. Combine my better micromarket algo to find better position opening and closing, maybe
// 7. Better road map
fn main() {
    let path = "./data/kline/klines_train.csv";
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

    // Сколько предыдущих строк образуют "тренд" перед разворотом
    const HISTORY_LEN: usize = 15;

    // Минимальный разворот в процентах относительно последнего значения тренда
    const REVERSAL_PCT: f64 = 1.0;

    // Список найденных разворотов:
    // (индекс строки разворота, направление, последнее значение тренда, текущее значение, изменение в %)
    let recorded_moves: Vec<_> = model_features
        // Скользящие окна: 15 строк тренда + 1 текущая строка (где ищем разворот)
        .windows(HISTORY_LEN + 1)
        .filter_map(|window| {
            // Собираем sma_difference 15 строк истории; если хоть одно None, окно пропускаем
            let previous_values: Vec<f64> = window[..HISTORY_LEN]
                .iter()
                .map(|row| row.sma_difference)
                .collect::<Option<Vec<f64>>>()?;

            // Текущее значение (16-я строка окна); None -> окно пропускаем
            let current = window[HISTORY_LEN].sma_difference?;

            // Последнее значение тренда: от него считаем размер разворота
            let last = previous_values[HISTORY_LEN - 1];

            // Защита от деления на ноль в формуле процентов ниже
            if last == 0.0 {
                return None;
            }

            // Тренд вверх: каждое следующее значение >= предыдущего
            let is_rising = previous_values.windows(2).all(|pair| pair[1] >= pair[0]);

            // Тренд вниз: каждое следующее значение <= предыдущего
            let is_falling = previous_values.windows(2).all(|pair| pair[1] <= pair[0]);

            // Если оба флага равны:
            //  - оба true  -> все значения одинаковы (плоская линия, тренда нет)
            //  - оба false -> значения колеблются вверх-вниз (тренда нет)
            if is_rising == is_falling {
                return None;
            }

            // Изменение текущего значения относительно последнего значения тренда, в %:
            // (текущее - последнее) / |последнее| * 100
            // abs() в знаменателе сохраняет знак изменения при отрицательном last
            let change_pct = (current - last) / last.abs() * 100.0;

            // Разворот: тренд был вверх, а значение упало на REVERSAL_PCT и больше
            // либо тренд был вниз, а значение выросло на REVERSAL_PCT и больше
            let direction = if is_rising && change_pct <= -REVERSAL_PCT {
                "up->down"
            } else if is_falling && change_pct >= REVERSAL_PCT {
                "down->up"
            } else {
                return None;
            };

            // Индекс берём у текущей строки (window[15]), где произошёл разворот
            Some((
                window[HISTORY_LEN].index,
                direction,
                last,
                current,
                change_pct,
            ))
        })
        .collect();

    // let recorded_moves: Vec<_> = model_features
    //     .windows(window_len + 1)
    //     .filter_map(|window| {
    //         let previous_values: Option<Vec<f64>> = window[..window_len]
    //             .iter()
    //             .map(|row| row.sma_difference)
    //             .collect();

    //         let previous_values = previous_values?;
    //         let previous_avg = previous_values.iter().sum::<f64>() / 15.0;
    //         let current = window[window_len].sma_difference?;

    //         if previous_avg == 0.0 {
    //             return None;
    //         }

    //         let change_pct = (current - previous_avg) / previous_avg.abs() * 100.0;

    //         (change_pct >= 25.0).then_some((
    //             window[window_len].index,
    //             previous_avg,
    //             current,
    //             change_pct,
    //         ))
    //     })
    //     .collect();

    // for (index, previous, current, change_pct) in &recorded_moves {
    //     println!(
    //         "Свеча {index}: SMA difference выросла с {previous:.6} до {current:.6} ({change_pct:.2}%)"
    //     );
    // }

    // #[derive(Clone, Copy)]
    // enum Trend {
    //     Rising,
    //     Falling,
    // }

    // const REVERSAL_THRESHOLD_PCT: f64 = 100.0;
    // let mut recorded_moves: Vec<(usize, f64, f64, f64)> = Vec::new();
    // let mut trend: Option<Trend> = None;
    // let mut candidate: Option<(usize, f64)> = None;

    // // let signals = model_features
    // //     .windows(16)
    // //     .filter_map(|window| {
    // //         let previous_values = window[..5].iter().map(|row| row.sma_difference).collect();
    // //     })
    // //     .collect();

    // for row in &model_features {
    //     let Some(value) = row.sma_difference else {
    //         continue;
    //     };

    //     let Some((_, candidate_value)) = candidate else {
    //         candidate = Some((row.index, value));
    //         continue;
    //     };

    //     match trend {
    //         None => {
    //             if value > candidate_value {
    //                 trend = Some(Trend::Rising);
    //                 candidate = Some((row.index, value));
    //             } else if value < candidate_value {
    //                 trend = Some(Trend::Falling);
    //                 candidate = Some((row.index, value));
    //             }
    //         }

    //         Some(Trend::Rising) => {
    //             if value >= candidate_value {
    //                 candidate = Some((row.index, value));
    //             } else if candidate_value != 0.0 {
    //                 let reversal_pct = (candidate_value - value) / candidate_value.abs() * 100.0;

    //                 if reversal_pct >= REVERSAL_THRESHOLD_PCT {
    //                     // Верхний разворот: отрицательный change_pct.
    //                     // Точка графика — текущая свеча, когда разворот подтвердился.
    //                     recorded_moves.push((row.index, candidate_value, value, -reversal_pct));

    //                     trend = Some(Trend::Falling);
    //                     candidate = Some((row.index, value));
    //                 }
    //             }
    //         }

    //         Some(Trend::Falling) => {
    //             if value <= candidate_value {
    //                 candidate = Some((row.index, value));
    //             } else if candidate_value != 0.0 {
    //                 let reversal_pct = (value - candidate_value) / candidate_value.abs() * 100.0;

    //                 if reversal_pct >= REVERSAL_THRESHOLD_PCT {
    //                     // Нижний разворот: положительный change_pct.
    //                     recorded_moves.push((row.index, candidate_value, value, reversal_pct));

    //                     trend = Some(Trend::Rising);
    //                     candidate = Some((row.index, value));
    //                 }
    //             }
    //         }
    //     }
    // }

    println!(
        "sma difference 25% move in 15 candles {}",
        recorded_moves.len()
    );

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
        3000,
        3500,
        &recorded_moves,
    );
}
