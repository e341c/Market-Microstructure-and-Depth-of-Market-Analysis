mod linear_regression;

use market_microstructure::{config::get_config, fetchdata};
use std::error::Error;

use crate::linear_regression::linear_regression;

fn main() -> Result<(), Box<dyn Error>> {
    let cfg = get_config();
    let interval = "5m";
    let limit = "10";

    let klines = fetchdata::fetchklines(&cfg, interval, limit)?;

    for kline in &klines {
        println!(
            "Open Time: {} | Open: {} | Close: {} | Volume: {}",
            kline.open_time, kline.open, kline.close, kline.volume
        );
    }

    let Some((test, train)) = klines.split_last() else {
        println!("Недостаточно данных");
        return Ok(());
    };

    let parsed_train: Vec<(f64, f64, f64)> = train
        .iter()
        .filter_map(|k| {
            Some((
                k.open.parse().ok()?,
                k.volume.parse().ok()?,
                k.close.parse().ok()?,
            ))
        })
        .collect();

    let x_raw = vec![
        parsed_train.iter().map(|(open, _, _)| *open).collect(),
        parsed_train.iter().map(|(_, volume, _)| *volume).collect(),
    ];

    let y: Vec<Vec<f64>> = parsed_train
        .iter()
        .map(|(_, _, close)| vec![*close])
        .collect();

    let Some(w) = linear_regression(&x_raw, &y) else {
        println!("Не удалось обучить модель");
        return Ok(());
    };

    let (Ok(open), Ok(volume), Ok(actual_close)) = (
        test.open.parse::<f64>(),
        test.volume.parse::<f64>(),
        test.close.parse::<f64>(),
    ) else {
        println!("Не удалось разобрать данные тестовой свечи");
        return Ok(());
    };

    let prediction = w[0] + w[1] * open + w[2] * volume;
    println!("Факт: {actual_close}, прогноз: {prediction}");

    Ok(())
}
