use std::io::{ Read, Write };
use std::net::{ TcpListener, TcpStream };
use sha1::{ Sha1, Digest };
use base64::{ prelude::BASE64_STANDARD, Engine };

const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

fn handle_client(mut stream: TcpStream) {
    let mut buffer = [0; 2048];
    if let Ok(bytes_read) = stream.read(&mut buffer) {
        let request = String::from_utf8_lossy(&buffer[..bytes_read]);

        // Ищем заголовок Sec-WebSocket-Key
        if let Some(key_line) = request.lines().find(|l| l.starts_with("Sec-WebSocket-Key:")) {
            let key = key_line.split(':').nth(1).unwrap().trim();

            // Вычисляем accept-ключ
            let mut hasher = Sha1::new();
            hasher.update(format!("{}{}", key, WS_GUID).as_bytes());
            let result = hasher.finalize();
            let accept_key = BASE64_STANDARD.encode(result);

            // Формируем HTTP-ответ переключения протокола
            let response =
                format!("HTTP/1.1 101 Switching Protocols\r\n\
                Upgrade: websocket\r\n\
                Connection: Upgrade\r\n\
                Sec-WebSocket-Accept: {}\r\n\r\n", accept_key);

            stream.write_all(response.as_bytes()).unwrap();
            println!("Handshake успешно завершен! Соединение переведено в WebSocket.");

            // Переходим к чтению WebSocket фреймов
            listen_frames(stream);
        }
    }
}

fn listen_frames(mut stream: TcpStream) {
    let mut header = [0; 2];

    // Бесконечный цикл чтения входящих сообщений
    while stream.read_exact(&mut header).is_ok() {
        let byte1 = header[0];
        let byte2 = header[1];

        let fin = (byte1 & 0b1000_0000) != 0;
        let opcode = byte1 & 0b0000_1111;
        let masked = (byte2 & 0b1000_0000) != 0;
        let mut payload_len = (byte2 & 0b0111_1111) as usize;

        // Обработка закрытия соединения клиентом
        if opcode == 0x8 {
            println!("Клиент закрыл соединение.");
            break;
        }

        // Упрощение: обрабатываем только короткие сообщения (< 126 байт)
        if payload_len == 126 || payload_len == 127 {
            println!("Реализация расширенной длины опущена для простоты примера.");
            break;
        }

        // Читаем маску (4 байта), если флаг выставлен
        let mut mask = [0; 4];
        if masked {
            stream.read_exact(&mut mask).unwrap();
        }

        // Читаем полезную нагрузку (payload)
        let mut payload = vec![0; payload_len];
        stream.read_exact(&mut payload).unwrap();

        // Демаскируем данные с помощью операции XOR
        if masked {
            for i in 0..payload_len {
                payload[i] ^= mask[i % 4];
            }
        }

        // Если это текстовый фрейм, выводим его
        if opcode == 0x1 {
            if let Ok(text) = String::from_utf8(payload) {
                println!("Получено сообщение: {}", text);
            }
        }
    }
}
