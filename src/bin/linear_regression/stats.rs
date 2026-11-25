use market_microstructure::fetchdata::Kline;

fn close_price(kline: &Kline) -> Option<f64> {
    kline.close.parse::<f64>().ok()
}

pub fn print_movement_stats(klines: &[Kline], interval_minutes: usize) {
    let closes: Vec<Option<f64>> = klines.iter().map(close_price).collect();

    for horizon_minutes in [15, 30, 60] {
        if horizon_minutes % interval_minutes != 0 {
            println!(
                "Пропуск горизонта {horizon_minutes}m: он не делится на интервал свечи {interval_minutes}m"
            );
            continue;
        }

        let steps = horizon_minutes / interval_minutes;
        let mut valid = 0;
        let mut up_20 = 0;
        let mut down_20 = 0;
        let mut up_30 = 0;
        let mut down_30 = 0;
        let mut direction_moves = 0;

        // i — свеча прогноза, i + steps — свеча через заданный горизонт.
        for i in 0..closes.len().saturating_sub(steps) {
            let (Some(start), Some(future)) = (closes[i], closes[i + steps]) else {
                continue;
            };

            if start == 0.0 {
                continue;
            }

            let change_bps = (future / start - 1.0) * 10_000.0;
            valid += 1;

            if change_bps > 20.0 {
                up_20 += 1;
            } else if change_bps < -20.0 {
                down_20 += 1;
            }

            if change_bps > 30.0 {
                up_30 += 1;
            } else if change_bps < -30.0 {
                down_30 += 1;
            }

            if change_bps != 0.0 {
                direction_moves += 1;
            }
        }

        if valid == 0 {
            println!("Нет валидных данных для горизонта {horizon_minutes}m");
            continue;
        }

        let pct = |count: usize| count as f64 / valid as f64 * 100.0;

        println!("\nГоризонт {horizon_minutes}m ({steps} свечей), наблюдений: {valid}");
        println!(
            "Движение > 20 б.п.: вверх {up_20} ({:.1}%), вниз {down_20} ({:.1}%)",
            pct(up_20),
            pct(down_20),
        );
        println!(
            "Движение > 30 б.п.: вверх {up_30} ({:.1}%), вниз {down_30} ({:.1}%)",
            pct(up_30),
            pct(down_30),
        );
        println!(
            "Ненулевое направление: {direction_moves} ({:.1}%)",
            pct(direction_moves),
        );
    }
}

#[derive(Debug, Clone, Copy)]
enum Direction {
    Up,
    Down,
    Flat,
}

fn direction(value_bps: f64) -> Direction {
    if value_bps > 0.0 {
        Direction::Up
    } else if value_bps < 0.0 {
        Direction::Down
    } else {
        Direction::Flat
    }
}

fn evaluate_direction(klines: &[Kline], interval_minutes: usize) {
    for (horizon_minutes, threshold_bps) in [(15, 20.0), (30, 20.0), (60, 20.0)] {
        let steps = horizon_minutes / interval_minutes;
        if steps == 0 {
            continue;
        }

        let mut evaluated = 0;
        let mut correct = 0;
        let mut wrong_move_bps_sum = 0.0;

        for i in 1..klines.len().saturating_sub(steps) {
            let (Some(prev), Some(current), Some(future)) = (
                klines[i - 1].close.parse::<f64>().ok(),
                klines[i].close.parse::<f64>().ok(),
                klines[i + steps].close.parse::<f64>().ok(),
            ) else {
                continue;
            };

            if prev == 0.0 || current == 0.0 {
                continue;
            }

            // Пример прогноза: направление последнего движения продолжится.
            let predicted_up = current > prev;
            let actual_change_bps = (future / current - 1.0) * 10_000.0;

            if actual_change_bps.abs() < threshold_bps {
                continue;
            }

            evaluated += 1;
            let actual_up = actual_change_bps > 0.0;

            if predicted_up == actual_up {
                correct += 1;
            } else {
                wrong_move_bps_sum += actual_change_bps.abs();
            }
        }

        let wrong = evaluated - correct;
        if evaluated > 0 {
            println!(
                "{horizon_minutes}m: проверено {evaluated}, точность {:.1}%, средний промах {:.1} б.п.",
                correct as f64 / evaluated as f64 * 100.0,
                if wrong > 0 {
                    wrong_move_bps_sum / wrong as f64
                } else {
                    0.0
                }
            );
        }
    }
}
