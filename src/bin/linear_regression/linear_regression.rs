// pub fn find() {
//     let x_arr: Vec<f64> = vec![1.0, 2.0, 3.0];
//     let y_arr: Vec<f64> = vec![2.0, 3.0, 7.0];

//     let mut x_sum: f64 = 0.0;
//     for x in &x_arr {
//         x_sum = x_sum + x
//     }

//     let mut y_sum: f64 = 0.0;
//     for y in &y_arr {
//         y_sum = y_sum + y
//     }

//     let x_median = x_sum / (x_arr.len() as f64);
//     let y_median: f64 = y_sum / (y_arr.len() as f64);

//     let mut b: f64 = 0.0;
//     let mut x_devider: f64 = 0.0;
//     for x in &x_arr {
//         let x_factor = x - x_median;
//         x_devider = x_devider + (x - x_median).powf(2.0);
//         let mut xy_factor = 0.0;
//         for y in &y_arr {
//             let y_factor = y - y_median;
//             xy_factor = xy_factor + (x_factor * y_factor);
//             b = b + (xy_factor / x_devider);
//         }
//     }
//     println!("{}", b);

//     let a: f64 = y_median - b * x_median;

//     println!("{}", a);

//     let Y: f64 = a + b * 4.0;

//     println!("{}", Y);
// }

use std::io;

use market_microstructure::fetchdata::Kline;

pub fn linear_regression(x_raw: &[Vec<f64>], y: &[Vec<f64>]) -> Option<Vec<f64>> {
    let n_samples = y.len();
    if n_samples == 0 || x_raw.is_empty() {
        return None;
    }

    // 1. Автоматически добавляем столбец единиц (intercept) в начало матрицы X,
    // Если x_raw уже имеет нужный размер, мы его копируем и дополняем едиками.
    let mut x: Vec<Vec<f64>> = Vec::with_capacity(x_raw.len() + 1);

    // Первая строка — единицы
    x.push(vec![1.0; n_samples]);

    // Остальные строки — исходные признаки
    for row in x_raw {
        x.push(row.clone());
    }

    // 2. Транспонируем матрицу X (получаем X^T / xv)
    let mut xv: Vec<Vec<f64>> = Vec::new();
    if let Some(first_row) = x.first() {
        for j in 0..first_row.len() {
            let mut column: Vec<f64> = Vec::new();
            for row in &x {
                column.push(row[j]);
            }
            xv.push(column);
        }
    }

    // X * X^T: матрица 3 × 3
    let xt = multiply_matrices(&x, &xv)?;

    // X * Y: вектор 3 × 1
    let xy = multiply_matrices(&x, y)?;

    let xyv: Vec<f64> = xy.iter().map(|row| row[0]).collect();
    let w = solve_linear_system(xt, xyv)?;

    Some(w)
}

fn multiply_matrices(a: &[Vec<f64>], b: &[Vec<f64>]) -> Option<Vec<Vec<f64>>> {
    let rows_a = a.len();
    if rows_a == 0 {
        return Some(vec![]);
    }
    let cols_a = a[0].len();

    let rows_b = b.len();
    if rows_b == 0 {
        return Some(vec![]);
    }
    let cols_b = b[0].len();

    // Проверка совместимости размеров для умножения
    if cols_a != rows_b {
        return None;
    }

    // Создаем результирующую матрицу размера (rows_a x cols_b), заполненную нулями
    let mut c = vec![vec![0.0; cols_b]; rows_a];

    // Классическое тройное циклическое умножение
    for i in 0..rows_a {
        for j in 0..cols_b {
            let mut sum = 0.0;
            for k in 0..cols_a {
                sum += a[i][k] * b[k][j];
            }
            c[i][j] = sum;
        }
    }

    Some(c)
}

fn solve_linear_system(mut a: Vec<Vec<f64>>, b: Vec<f64>) -> Option<Vec<f64>> {
    let n = a.len();

    // Создаем расширенную матрицу [A | B]
    for i in 0..n {
        a[i].push(b[i]);
    }

    // println!("Создаем расширенную матрицу [A | B] {:?}", a);

    // Прямой ход метода Гаусса с выбором главного элемента
    for i in 0..n {
        // Ищем строку с максимальным элементом в текущем столбце для точности
        let mut max_row = i;
        for k in i + 1..n {
            if a[k][i].abs() > a[max_row][i].abs() {
                max_row = k;
            }
        }
        a.swap(i, max_row);

        // Проверка на вырожденность матрицы
        if a[i][i].abs() < 1e-9 {
            return None;
        }

        // Обнуляем элементы ниже главной диагонали
        for k in i + 1..n {
            let factor = a[k][i] / a[i][i];
            for j in i..=n {
                a[k][j] -= factor * a[i][j];
            }
        }
    }

    // Обратный ход (back substitution)
    let mut w = vec![0.0; n];
    for i in (0..n).rev() {
        let mut sum = a[i][n];
        for j in i + 1..n {
            sum -= a[i][j] * w[j];
        }
        w[i] = sum / a[i][i];
    }

    Some(w)
}

// TODO: Add more features, add features builder, make more flexible
// TODO: separete x from y better, for now it is hard to add features because it has close price inside parsed train
pub fn evaluate_model(klines: &Vec<Kline>, seq_len: usize) -> Result<(), io::Error> {
    let seqs = split_to_seq(klines, seq_len).expect("cannot get seqs");
    println!("Seq len is {}", seqs.len());

    let mut model_abs_error_sum = 0.0;
    let mut baseline_abs_error_sum = 0.0;
    let mut prediction_count = 0;
    for seq in seqs {
        let split_at = seq.len() - 1;

        let (train, test) = seq.split_at(split_at);

        let parsed_train: Vec<(f64, f64, f64, f64, f64, f64, f64)> = train
            .iter()
            .filter_map(|k| {
                Some((
                    k.open.parse().ok()?,
                    k.high.parse().ok()?,
                    k.low.parse().ok()?,
                    k.volume.parse().ok()?,
                    k.close.parse().ok()?,
                    k.taker_buy_base_asset_volume.parse().ok()?,
                    k.taker_buy_quote_asset_volume.parse().ok()?,
                    // k.number_of_trades,
                ))
            })
            .collect();

        if parsed_train.len() < 2 || test.is_empty() {
            continue;
        }
        let pair_count = parsed_train.len() - 1;

        let x_raw = vec![
            parsed_train[..pair_count].iter().map(|r| r.0).collect(),
            parsed_train[..pair_count].iter().map(|r| r.1).collect(),
            parsed_train[..pair_count].iter().map(|r| r.2).collect(),
            parsed_train[..pair_count].iter().map(|r| r.3).collect(),
            parsed_train[..pair_count].iter().map(|r| r.5).collect(),
            parsed_train[..pair_count].iter().map(|r| r.6).collect(),
        ];

        let y: Vec<Vec<f64>> = parsed_train[1..]
            .iter()
            .map(|r| vec![r.4]) // close следующей свечи
            .collect();

        let Some(w) = linear_regression(&x_raw, &y) else {
            println!("Не удалось обучить модель");
            return Ok(());
        };

        println!("W is {:?}", w);

        // Признаки 10-й свечи прогнозируют close 11-й.
        let last = parsed_train.last().expect("Cannot get last");

        // Попробовать считать признаки через медиану, условно 10 секвенций, 10 результатов, находим среднию каждого признака
        // w[0] — intercept
        // w[1] — open
        // w[2] — high
        // w[3] — low
        // w[4] — volume
        // w[5] — taker_buy_base_asset_volume
        // w[6] — taker_buy_quote_asset_volume
        let prediction = w[0]
            + w[1] * last.0
            + w[2] * last.1
            + w[3] * last.2
            + w[4] * last.3
            + w[5] * last.5
            + w[6] * last.6;

        let actual_close = test[0].close;
        // let Ok(actual_close) = test[0].close.parse::<f64>() else {
        //     println!("Не удалось разобрать close тестовой свечи");
        //     return Ok(());
        // };

        // let close_time_test = test[0].close_time;
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

fn split_to_seq<T>(data: &Vec<T>, seq_len: usize) -> Result<Vec<Vec<&T>>, io::Error> {
    if seq_len <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "seq_len must be greater than zero",
        ));
    }

    let mut res: Vec<Vec<&T>> = Vec::new();
    let mut iter = data.into_iter();

    loop {
        let chunk: Vec<&T> = iter.by_ref().take(seq_len).collect();
        if chunk.is_empty() {
            break;
        }
        res.push(chunk);
    }

    Ok(res)
}
