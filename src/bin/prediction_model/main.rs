mod linear_regression;

use market_microstructure::{config::get_config, fetchdata};
use std::{error::Error, io};

use crate::linear_regression::linear_regression;

fn main() -> Result<(), Box<dyn Error>> {
    let cfg = get_config();
    let interval = "15m";
    let limit = "120";

    let klines = fetchdata::fetchklines(&cfg, interval, limit)?;

    // for kline in &klines {
    //     println!(
    //         "Open Time: {} | Open: {} | Close: {} | Volume: {} | Close time at: {}",
    //         kline.open_time, kline.open, kline.close, kline.volume, kline.close_time
    //     );
    // }
    println!("klines len is {}", klines.len());

    if klines.len() < 3 {
        println!("Недостаточно данных");
        return Ok(());
    }

    let seqs = split_to_seq(klines, 12).expect("cannot get seqs");

    let mut model_abs_error_sum = 0.0;
    let mut baseline_abs_error_sum = 0.0;
    let mut prediction_count = 0;
    for seq in seqs {
        let split_at = seq.len() - 2;

        let (train, test) = seq.split_at(split_at);

        let parsed_train: Vec<(f64, f64, f64, f64, f64)> = train
            .iter()
            .filter_map(|k| {
                Some((
                    k.open.parse().ok()?,
                    k.high.parse().ok()?,
                    k.low.parse().ok()?,
                    k.volume.parse().ok()?,
                    k.close.parse().ok()?,
                    // k.taker_buy_base_asset_volume.parse().ok()?,
                    // k.taker_buy_quote_asset_volume.parse().ok()?,
                    // k.number_of_trades,
                ))
            })
            .collect();

        // Для 10 обучающих свечей создаём 9 пар:
        // признаки свечи i предсказывают close свечи i + 1.
        let x_raw = vec![
            parsed_train[..9].iter().map(|r| r.0).collect(), // open
            parsed_train[..9].iter().map(|r| r.1).collect(), // high
            parsed_train[..9].iter().map(|r| r.2).collect(), // low
            parsed_train[..9].iter().map(|r| r.3).collect(), // volume
        ];

        let y: Vec<Vec<f64>> = parsed_train[1..]
            .iter()
            .map(|r| vec![r.4]) // close следующей свечи
            .collect();

        let Some(w) = linear_regression(&x_raw, &y) else {
            println!("Не удалось обучить модель");
            return Ok(());
        };

        // Признаки 10-й свечи прогнозируют close 11-й.
        let last = parsed_train.last().expect("Cannot get last");

        // w[0] — intercept
        // w[1] — open
        // w[2] — high
        // w[3] — low
        // w[4] — volume
        let prediction = w[0] + w[1] * last.0 + w[2] * last.1 + w[3] * last.2 + w[4] * last.3;

        let Ok(actual_close) = test[0].close.parse::<f64>() else {
            println!("Не удалось разобрать close тестовой свечи");
            return Ok(());
        };

        let close_time_test = test[0].close_time;
        println!("\nW is {:?}", w);

        // Baseline: predict that the next close equals the last training close.
        let baseline_prediction = last.4;

        let model_error = (actual_close - prediction).abs();
        let baseline_error = (actual_close - baseline_prediction).abs();

        model_abs_error_sum += model_error;
        baseline_abs_error_sum += baseline_error;
        prediction_count += 1;

        println!("Fact: {actual_close:.2}");
        println!("Model: {prediction:.2} | abs error: {model_error:.2}");
        println!("Baseline: {baseline_prediction:.2} | abs error: {baseline_error:.2}");
    }

    if prediction_count > 0 {
        println!(
            "\nModel MAE: {:.2}",
            model_abs_error_sum / prediction_count as f64
        );
        println!(
            "Baseline MAE: {:.2}",
            baseline_abs_error_sum / prediction_count as f64
        );
    }

    Ok(())
}

fn split_to_seq<T>(data: Vec<T>, seq_len: usize) -> Result<Vec<Vec<T>>, io::Error> {
    if seq_len <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "seq_len must be greater than zero",
        ));
    }

    let mut res: Vec<Vec<T>> = Vec::new();
    let mut iter = data.into_iter();

    loop {
        let chunk: Vec<T> = iter.by_ref().take(seq_len).collect();
        if chunk.is_empty() {
            break;
        }
        res.push(chunk);
    }

    Ok(res)
}
