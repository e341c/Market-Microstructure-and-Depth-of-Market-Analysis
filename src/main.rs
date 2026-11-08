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
use std::time::{Duration, Instant};

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

    ws::client::run_client(&cfg.binance_ws_url);

    loop {
        if !session.read_some()? {
            eprintln!("server closed connection");
            break;
        }

        while let Some(msg) = session.next_msg() {
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
                    let received = Instant::now();
                    book.apply(&msg, mt == "W");
                    let Some(seq) = get(&msg, 34).and_then(|s| s.parse::<u64>().ok()) else {
                        continue;
                    };
                    if let Some(top) = book.top() {
                        sim.update_market_state(seq, &top, received);
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
