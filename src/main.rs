mod auth;
mod book;
mod config;
mod file;
mod fix;
mod fixed;
mod metrics;
mod parser;
mod simulator;
mod timeutil;
mod ws;

use book::Book;
use config::get_config;
use fix::message::get;
use fix::session::Session;
use fixed::{fmt_fixed, fmt_obi};
use simulator::HftSimulator;
use std::{
    collections::VecDeque,
    fs::{File, OpenOptions},
    io::{BufWriter, Write},
    time::{Duration, Instant},
};

use crate::{book::Top, timeutil::utc_timestamp};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = get_config();
    let pem = std::fs::read_to_string(&cfg.pem_path)?;
    let key = auth::load_key_from_pem(&pem)?;

    let mut sim = HftSimulator::new();

    let mut session = Session::connect(&cfg.addr, "BOOKBOT", "SPOT")?;
    session.logon(&cfg.api_key, &key, cfg.heartbeat_secs)?;

    let mut book = Book::default();
    let mut subscribed = false;
    let mut last_sent = Instant::now();
    let hb_interval = Duration::from_secs(cfg.heartbeat_secs.saturating_sub(5).max(1) as u64);

    // ws::client::run_client(&cfg.binance_ws_url);

    // ofi_window: &mut VecDeque<(Instant, i128)>,
    // obi_window: &mut VecDeque<(Instant, i32)>,

    // file writing
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open("metrics.log")
        .expect("cannot create file");
    let mut file = BufWriter::with_capacity(1 * 1024, file);

    let mut ofi_window: VecDeque<(Instant, i128)> = VecDeque::new();
    let mut obi_window: VecDeque<(Instant, i32)> = VecDeque::new();

    let mut prev_top: Option<Top> = None;

    loop {
        if !session.read_some()? {
            eprintln!("server closed connection");
            break;
        }

        while let Some(msg) = session.next_msg() {
            let received = Instant::now();
            match get(&msg, 35).unwrap_or("") {
                "A" => {
                    println!("logged on");
                    if !subscribed {
                        session.subscribe(&cfg.symbol, cfg.depth)?;
                        subscribed = true;
                    }
                }
                "1" => {
                    let id = get(&msg, 112).unwrap_or("").to_string();
                    session.send("0", &[(112, id)])?;
                    last_sent = Instant::now();
                }
                mt @ ("W" | "X") => {
                    book.apply(&msg, mt == "W");
                    let Some(seq) = get(&msg, 34).and_then(|s| s.parse::<u64>().ok()) else {
                        continue;
                    };
                    if let Some(top) = book.top() {
                        // HFT simulator
                        // sim.update_market_state(seq, &top, received);

                        let p_top = prev_top.as_ref().unwrap_or(&top);

                        let microprice = metrics::microprice(&top) as u128;

                        // OBI calc logic
                        let obi = metrics::obi(&top) as i32;
                        obi_window.push_back((received, obi));

                        while let Some((timestamp, _)) = obi_window.front() {
                            if received.duration_since(*timestamp) >= Duration::from_secs(1) {
                                obi_window.pop_front();
                            } else {
                                break;
                            }
                        }

                        let obi1s: i32 = if obi_window.is_empty() {
                            0
                        } else {
                            (obi_window
                                .iter()
                                .map(|(_, value)| *value as i64)
                                .sum::<i64>()
                                / obi_window.len() as i64) as i32
                        };

                        // OFI calc logic
                        let ofi = metrics::ofi(&top, p_top);

                        // Добавляем текущее значение
                        ofi_window.push_back((received, ofi));

                        // Удаляем события старше 500 ms
                        while let Some((timestamp, _)) = ofi_window.front() {
                            if received.duration_since(*timestamp) >= Duration::from_millis(500) {
                                ofi_window.pop_front();
                            } else {
                                break;
                            }
                        }

                        // Суммируем текущие события в окне
                        let ofi500: i128 = ofi_window.iter().map(|(_, value)| *value).sum();

                        let sending_time = get(&msg, 52).unwrap_or("");
                        let latency = received.elapsed().as_micros();
                        let log = format!(
                            "{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{};",
                            latency,
                            utc_timestamp(),
                            sending_time,
                            seq,
                            fmt_fixed(top.pa.price()),
                            fmt_fixed(top.pb.price()),
                            fmt_fixed(microprice),
                            obi,
                            obi1s,
                            ofi,
                            ofi500,
                        );
                        writeln!(file, "{}", log).expect("cannot write log");
                        prev_top = Some(top.clone());
                    }
                }
                "3" | "j" | "5" => {
                    eprintln!("reject/logout: {:?}", msg);
                    return Ok(());
                }
                _ => {}
            }
        }

        if last_sent.elapsed() >= hb_interval {
            session.send("0", &[])?;
            last_sent = Instant::now();
        }
    }
    Ok(())
}

// Повторяющиеся ±20.8 млн (seq 9, 28, 39, 42, 51, 76, 77, 86, 87). Это один и тот же объём около 0.208 BTC, который то появляется, то исчезает на топе: либо чей-то алгоритм мигает заявкой, либо это одна крупная заявка. На такие события приходится заметная часть ofi500, так что при анализе сигналов их стоит учитывать.
// Масштаб. ofi измеряется в единицах объёма ×1e8, то есть 834925000 = 8.35 BTC. Для графиков и порогов удобно привести к BTC или нормировать.
// Скачки цены на 0.20 и 2.14 (seq 89–90) дают очень большие значения, потому что при сдвиге цены в OFI целиком входит объём уровня. Так и задумано в формуле Cont et al., но на спайках ofi будет выбросом, и это важно для статистики (например, лучше использовать ранги или обрезку).
// Паузы и пачки. Паузы 190 мс (47.749 → 47.941) и 280 мс (48.541 → 48.818) по-прежнему есть, а пачка из seq 50–54 с интервалом около 100–400 мкс говорит о том, что сообщения пришли одним пакетом. Для анализа тайминга лучше добавить время биржи (тег 52).
// obi1s по-прежнему среднее по событиям, а не по времени (см. мой прошлый комментарий).