use base64::{Engine, prelude::BASE64_STANDARD};
use rustls::{ClientConfig, RootCertStore, Stream};
use rustls_pki_types::ServerName;
use sha1::{Digest, Sha1};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;

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

    // 5. Читаем стрим от Binance
    println!("Ожидание данных от Binance...");
    loop {
        read_server_frame(&mut stream);
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

// Изменим чтение, чтобы оно могло обрабатывать фреймы больше 125 байт,
// так как маркет-дата от Binance всегда большая
fn read_server_frame<S: Read>(stream: &mut S) {
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

    // Обработка расширенной длины фреймов (Binance шлет крупные JSON)
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
        if opcode == 0x01 {
            // Текст
            if let Ok(text) = String::from_utf8(payload) {
                println!("Данные: {}", text);
            }
        } else if opcode == 0x9 {
            // Ping от сервера
            println!("Получен Ping от Binance");
            // В идеале тут нужно отправить обратно Pong (opcode 0xA)
        }
    }
}

fn generate_nonce() -> String {
    let mut nonce = [0u8; 16];
    rand::fill(&mut nonce);
    BASE64_STANDARD.encode(nonce)
}
