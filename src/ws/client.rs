use base64::{Engine, prelude::BASE64_STANDARD};
use rand::RngExt;
use rustls::{ClientConfig, RootCertStore, Stream};
use rustls_pki_types::ServerName;
use sha1::{Digest, Sha1};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::orderbook::{MAS, Oba, OrderBookCollection};
use crate::parser::{ExtractedData, parse_segmented_payload};
use crate::simulator::{self, HftSimulator};

const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

pub fn run_client(url: &String) {
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

    let mut simulator = simulator::HftSimulator::new(100_000_000);

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

    // 5. Читаем стрим от Binance
    println!("Ожидание данных от Binance...");
    loop {
        read_server_frame(&mut stream, &mut simulator);
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

fn read_server_frame<S: Read + Write>(stream: &mut S, simulator: &mut HftSimulator) {
    // УДАЛЯЕМ замер отсюда (он считал время ожидания сети)

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
        // СЮДА ПЕРЕНОСИМ ЗАМЕР: Данные физически у нас в памяти (в vec),
        // сеть больше не блокирует выполнение. Начинается чистая обработка!
        let packet_received_at = std::time::Instant::now();

        match opcode {
            0x01 => {
                if let Ok(text) = String::from_utf8(payload) {
                    let result: Option<ExtractedData<Oba>> =
                        parse_segmented_payload(&text, "lastUpdateId", "bids", "asks");

                    let empty_order = Oba([0, 0]);
                    let mut book = OrderBookCollection {
                        last_update_id: 0,
                        bid: [empty_order; MAS],
                        ask: [empty_order; MAS],
                        pa: empty_order,
                        pb: empty_order,
                    };

                    if let Some(extracted_data) = result {
                        book.last_update_id = extracted_data.id;
                        for (i, item) in extracted_data
                            .primary_list
                            .into_iter()
                            .take(MAS)
                            .enumerate()
                        {
                            book.bid[i] = item;
                        }
                        for (i, item) in extracted_data
                            .secondary_list
                            .into_iter()
                            .take(MAS)
                            .enumerate()
                        {
                            book.ask[i] = item;
                        }
                    } else {
                        eprintln!("Failed to parse orderbook segments from JSON");
                    }

                    let best_ask = book.pa();
                    let best_bid = book.pb();
                    let microprice = book.microprice();
                    let obi = book.obi();

                    // Передаем одинаковый Instant, так как замеряем чистую скорость выполнения кода
                    simulator.update_market_state(
                        book.last_update_id,
                        microprice,
                        obi,
                        best_ask.0[0],
                        best_bid.0[0],
                        packet_received_at, // Измеряет: Парсинг JSON + расчет метрик + симулятор
                    );
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
