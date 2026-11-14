use base64::{Engine, prelude::BASE64_STANDARD};
use rand::RngExt;
use rustls::{ClientConfig, RootCertStore, Stream};
use rustls_pki_types::ServerName;
use sha1::{Digest, Sha1};
use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use market_microstructure::book::{Book, Oba, Top};
use market_microstructure::fixed::fmt_fixed;
use market_microstructure::metrics;
use market_microstructure::parser::parse_segmented_payload;
use market_microstructure::timeutil::utc_timestamp;

const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

pub fn run_client(url: &str) {
    // 1. Более надежный парсинг URL (выделяем host, port и path)
    let clean_url = url.replace("wss://", "");

    // Ищем, где начинается путь (первый слеш после домена)
    let (host_and_port, path) = match clean_url.find('/') {
        Some(idx) => (&clean_url[..idx], &clean_url[idx..]),
        None => (clean_url.as_str(), "/ws"), // Дефолтный путь для Binance, если забыли указать
    };

    // Выделяем домен и порт
    let host_parts: Vec<&str> = host_and_port.split(':').collect();
    let domain = host_parts[0];
    let port = if host_parts.len() > 1 {
        host_parts[1]
    } else {
        "443"
    };
    let connection_string = format!("{}:{}", domain, port);

    // 2. Подключаемся по TCP
    let mut tcp = TcpStream::connect(&connection_string).expect(&format!(
        "Не удалось подключиться по TCP к {}",
        connection_string
    ));
    println!("TCP соединение установлено с {}", connection_string);

    // 3. Инициализируем TLS
    let mut root_store = RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    let config = ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();

    let server_name = ServerName::try_from(domain.to_string())
        .expect("Неверное имя домена для TLS")
        .to_owned();

    let mut tls_conn = rustls::ClientConnection::new(Arc::new(config), server_name)
        .expect("Не удалось создать TLS соединение");

    let mut stream = Stream::new(&mut tls_conn, &mut tcp);
    println!("TLS рукопожатие успешно завершено!");

    // 4. Выполняем WebSocket Handshake с правильным Path и Host
    let ws_key = generate_nonce();
    send_handshake(&mut stream, domain, path, &ws_key);
    verify_handshake(&mut stream, &ws_key);

    // ПРИМЕР ОТПРАВКИ СООБЩЕНИЯ ДЛЯ BINANCE:
    // Если вы изначально подключились просто к "wss://://binance.com",
    // вы можете динамически подписаться на любые стримы, отправив JSON:
    let subscribe_message = r#"{
        "method": "SUBSCRIBE",
        "params": [
            "btcusdt@depth20@100ms"
        ],
        "id": 1
    }"#;

    println!("Отправляем запрос на подписку...");
    send_text_frame(&mut stream, subscribe_message);

    // file writing
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open("metrics.log")
        .expect("cannot create file");

    // 5. Читаем стрим от Binance
    println!("Ожидание данных от Binance...");
    let mut book = Book::default();
    let mut prev_top: Option<Top> = None;
    // let mut ofi_arr = Vec::new();
    // let mut obi_arr = Vec::new();

    let mut ofi_window: VecDeque<(Instant, i128)> = VecDeque::new();
    let mut obi_window: VecDeque<(Instant, i32)> = VecDeque::new();
    loop {
        read_server_frame(
            &mut stream,
            &mut file,
            &mut book,
            &mut prev_top,
            &mut ofi_window,
            &mut obi_window,
        );
    }
}

fn send_handshake<S: Write>(stream: &mut S, host: &str, path: &str, ws_key: &str) {
    // ВАЖНО: GET должен идти на конкретный path, а Host содержать только домен
    let handshake = format!(
        "GET {} HTTP/1.1\r\n\
        Host: {}\r\n\
        Upgrade: websocket\r\n\
        Connection: Upgrade\r\n\
        Sec-WebSocket-Key: {}\r\n\
        Sec-WebSocket-Version: 13\r\n\r\n",
        path, host, ws_key
    );
    stream.write_all(handshake.as_bytes()).unwrap();
    stream.flush().unwrap();
}

fn verify_handshake<S: Read>(stream: &mut S, ws_key: &str) {
    let mut buffer = [0; 2048];
    let bytes_read = stream.read(&mut buffer).expect("Ошибка чтения Handshake");
    let response = String::from_utf8_lossy(&buffer[..bytes_read]);

    let mut hasher = Sha1::new();
    hasher.update(format!("{}{}", ws_key, WS_GUID).as_bytes());
    let expected_accept = BASE64_STANDARD.encode(hasher.finalize());

    let accept_header = response
        .lines()
        .find(|l| l.to_lowercase().starts_with("sec-websocket-accept:"));

    match accept_header {
        Some(header) => {
            let actual_accept = header.split(':').nth(1).unwrap().trim();
            if actual_accept == expected_accept {
                println!("WebSocket Handshake успешен! Соединение установлено.");
            } else {
                panic!("Ключ сервера не совпал!");
            }
        }
        None => {
            // Если заголовка нет, выводим сырой ответ сервера для отладки
            println!("--- ОТВЕТ СЕРВЕРА ---");
            println!("{}", response);
            println!("---------------------");
            panic!("Сервер не вернул Sec-WebSocket-Accept. См. ответ выше.");
        }
    }
}

fn read_server_frame<S: Read + Write>(
    stream: &mut S,
    file: &mut File,
    book: &mut Book,
    prev_top: &mut Option<Top>,
    ofi_window: &mut VecDeque<(Instant, i128)>,
    obi_window: &mut VecDeque<(Instant, i32)>,
) {
    let mut header = [0; 2];
    if stream.read_exact(&mut header).is_err() {
        return;
    }

    let byte1 = header[0];
    let byte2 = header[1];
    let opcode = byte1 & 0x0f;
    let mut payload_len = (byte2 & 0x7f) as usize;

    if opcode == 0x8 {
        println!("Сервер закрыл соединение.");
        std::process::exit(0);
    }

    if payload_len == 126 {
        let mut ext_len = [0; 2];
        stream.read_exact(&mut ext_len).unwrap();
        payload_len = u16::from_be_bytes(ext_len) as usize;
    } else if payload_len == 127 {
        let mut ext_len = [0; 8];
        stream.read_exact(&mut ext_len).unwrap();
        payload_len = u64::from_be_bytes(ext_len) as usize;
    }

    let mut payload = vec![0; payload_len];
    if stream.read_exact(&mut payload).is_ok() {
        let received = Instant::now();

        match opcode {
            0x01 => {
                if let Ok(text) = String::from_utf8(payload) {
                    if let Some(data) =
                        parse_segmented_payload::<Oba>(&text, "lastUpdateId", "bids", "asks")
                    {
                        book.apply_levels(&data.primary_list, &data.secondary_list);
                        if let Some(top) = book.top() {
                            // let p_top = prev_books.last().and_then(Option::as_ref).unwrap_or(&top);
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
                                    / obi_window.len() as i64)
                                    as i32
                            };

                            // OFI calc logic
                            let ofi = metrics::ofi(&top, p_top);

                            // Добавляем текущее значение
                            ofi_window.push_back((received, ofi));

                            // Удаляем события старше 500 ms
                            while let Some((timestamp, _)) = ofi_window.front() {
                                if received.duration_since(*timestamp) >= Duration::from_millis(500)
                                {
                                    ofi_window.pop_front();
                                } else {
                                    break;
                                }
                            }

                            // Суммируем текущие события в окне
                            let ofi500: i128 = ofi_window.iter().map(|(_, value)| *value).sum();

                            let latency = received.elapsed().as_micros();

                            // log is timestamp, data id, best ask, best bid, obi, ofi, microprice, duration in µs
                            let log = format!(
                                "{}|{}|{}|{}|{}|{}|{}|{}|{}|{};",
                                latency,
                                utc_timestamp(),
                                data.id,
                                fmt_fixed(top.pa.price()),
                                fmt_fixed(top.pb.price()),
                                fmt_fixed(microprice),
                                obi,
                                obi1s,
                                ofi,
                                ofi500,
                            );
                            writeln!(file, "{}", log).expect("cannot write log");
                            // sim.update_market_state(data.id, &top, received);
                            // prev_books.push(Some(top.clone()));
                            *prev_top = Some(top.clone());
                        }
                    }
                }
            }
            0x9 => {
                println!("Получен Ping от Binance");
                send_pong(stream, &payload);
            }
            _ => {}
        }
    }
}

// Отправка текстового фрейма ТЕПЕРЬ ПОДДЕРЖИВАЕТ ЛЮБУЮ ДЛИНУ (даже > 125 байт)
// так как JSON-запросы подписки могут быть длинными
fn send_text_frame<S: Write>(stream: &mut S, text: &str) {
    let payload = text.as_bytes();
    let payload_len = payload.len();
    let mut frame = Vec::new();

    // 1-й байт: FIN = 1 + Opcode = 1 (Текст) -> 0x81
    frame.push(0x81);

    // 2-й байт и далее: Кодируем длину согласно RFC 6455
    if payload_len <= 125 {
        // Длина помещается в 7 бит, не забываем включить бит маски (0x80)
        frame.push(0x80 | (payload_len as u8));
    } else if payload_len <= 65535 {
        // Длина помещается в 16 бит
        frame.push(0x80 | 126);
        frame.extend_from_slice(&(payload_len as u16).to_be_bytes());
    } else {
        // Длина помещается в 64 бита
        frame.push(0x80 | 127);
        frame.extend_from_slice(&(payload_len as u64).to_be_bytes());
    }

    // Генерируем маску (обязательно для клиента)
    let mut rng = rand::rng();
    let mask: [u8; 4] = rng.random();
    frame.extend_from_slice(&mask);

    // XOR-маскирование данных
    for (i, &byte) in payload.iter().enumerate() {
        frame.push(byte ^ mask[i % 4]);
    }

    stream.write_all(&frame).unwrap();
    stream.flush().unwrap();
    println!("-> Отправлено сообщение на сервер");
}

// Отправка Pong фрейма (обязательно маскированного!) в ответ на Ping
fn send_pong<S: Write>(stream: &mut S, ping_payload: &[u8]) {
    let mut frame = Vec::new();

    // 1-й байт: FIN = 1 + Opcode = 0xA (Pong) -> 0x8A
    frame.push(0x8A);

    // Пинг-понг фреймы обычно маленькие (до 125 байт)
    frame.push(0x80 | (ping_payload.len() as u8));

    let mut rng = rand::rng();
    let mask: [u8; 4] = rng.random();
    frame.extend_from_slice(&mask);

    for (i, &byte) in ping_payload.iter().enumerate() {
        frame.push(byte ^ mask[i % 4]);
    }

    stream.write_all(&frame).unwrap();
    stream.flush().unwrap();
    println!("-> Отправлен Pong в ответ на Ping от сервера.");
}

fn generate_nonce() -> String {
    let mut nonce = [0u8; 16];
    rand::fill(&mut nonce);
    BASE64_STANDARD.encode(nonce)
}

fn get_full_timestamp() -> String {
    let now = SystemTime::now();
    let since_the_epoch = now.duration_since(UNIX_EPOCH).unwrap();
    let secs = since_the_epoch.as_secs();
    let millis = since_the_epoch.subsec_millis();

    // Упрощенный перевод Unix-времени в формат ЧЧ:ММ:СС (по UTC для HFT)
    let hour = (secs % 86400) / 3600;
    let minute = (secs % 3600) / 60;
    let second = secs % 60;

    format!("{:02}:{:02}:{:02}.{:03}", hour, minute, second, millis)
}
