//! Агрегирует сырые aggTrades (один CSV-файл или каталог CSV, с заголовком или без) в минутные
//! потоковые агрегаты flow_1m.csv.
//! Запуск: cargo run --release --bin aggregate_flow aggTrade.csv flow_1m.csv

use std::collections::BTreeMap;
use std::env;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use market_microstructure::intraday_flow::{MINUTE_MS, MinuteFlow, parse_agg_line, write_flow_csv};

// use intraday_flow::{MINUTE_MS, MinuteFlow, parse_agg_line, write_flow_csv};

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

/// Собирает список CSV: если путь - файл, берётся он один; если каталог - все *.csv,
/// отсортированные по имени (для имён <timestamp>.csv это порядок по времени).
fn collect_csv_files(path: &str) -> Result<Vec<PathBuf>, String> {
    let p = Path::new(path);
    if p.is_file() {
        return Ok(vec![p.to_path_buf()]);
    }
    let mut files: Vec<PathBuf> = fs::read_dir(p)
        .map_err(|e| format!("cannot read {path}: {e}"))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|x| x.extension().map_or(false, |ext| ext == "csv"))
        .collect();
    files.sort();
    Ok(files)
}

/// Читает все файлы построчно (одним переиспользуемым буфером, без загрузки файла в память),
/// накапливает агрегаты по минутам в BTreeMap (ключ - номер минуты) и пишет результат.
/// Печатает диапазон времени потока и число пропущенных минут внутри диапазона:
/// gaps = (последняя_минута - первая_минута + 1) - число_минут_с_данными.
fn run() -> Result<(), String> {
    let input = env::args()
        .nth(1)
        .unwrap_or_else(|| "aggTrade.csv".to_string());
    let out = env::args()
        .nth(2)
        .unwrap_or_else(|| "flow_1m.csv".to_string());
    let files = collect_csv_files(&input)?;
    if files.is_empty() {
        return Err(format!("no csv files in {input}"));
    }

    let mut flow: BTreeMap<i64, MinuteFlow> = BTreeMap::new();
    let (mut parsed, mut skipped) = (0u64, 0u64);
    for path in &files {
        let file = File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        let mut reader = BufReader::with_capacity(1 << 20, file); // буфер 1 МБ
        let mut buf = String::new();
        loop {
            buf.clear();
            let n = reader.read_line(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break; // конец файла
            }
            match parse_agg_line(&buf) {
                Some((minute, price, qty, fills, buyer_is_maker)) => {
                    flow.entry(minute)
                        .or_default()
                        .add_trade(price, qty, fills, buyer_is_maker);
                    parsed += 1;
                }
                None => skipped += 1, // заголовок или битая строка
            }
        }
    }
    let first = *flow.keys().next().ok_or("no trades parsed")?;
    let last = *flow.keys().next_back().ok_or("no trades parsed")?;
    // Пропуски внутри диапазона: ожидаемое число минут минус найденное
    let gaps = (last - first + 1) as usize - flow.len();
    write_flow_csv(&out, &flow)?;
    println!(
        "files={}, trades={parsed}, skipped_lines={skipped}, minutes={}, gaps={gaps}",
        files.len(),
        flow.len()
    );
    println!(
        "диапазон потока: [{}, {}] мс; saved: {out}",
        first * MINUTE_MS,
        last * MINUTE_MS
    );
    Ok(())
}
