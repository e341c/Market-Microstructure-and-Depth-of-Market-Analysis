mod orderbook;
use orderbook::{ Oba, OrderBookCollection, MAS };

pub mod config;
use config::{ get_config };

pub mod fetchdata;
use fetchdata::{ fetchdata };

pub mod parser;
use parser::{ ExtractedData, parse_segmented_payload };

// NEXT STEPS:
// Get config
// Fetch data for api
// Create order book collection based on that data
// Find Pa and Pb for order book collection
// Find OBI (Order Flow Imbalance) (дисбаланс стакана) = (Vb − Va) / (Vb + Va), от −1 до +1.
// Find Microprice = (Pa·Vb + Pb·Va) / (Vb + Va).
// Find Market Memory and Iceberg
// Set Model for ML
// Квантование стакана ?
// Адверс-селекция и VPIN
fn main() {
    let config = get_config();
    println!("{}", config.binance_url);
    let raw_json = fetchdata(config).expect("failed to fetch data");

    let result: Option<ExtractedData<Oba>> = parse_segmented_payload(
        &raw_json,
        "lastUpdateId",
        "bids",
        "asks"
    );

    // Исправлено: используем {:?}, так как у ExtractedData есть только #[derive(Debug)]
    println!("{:?}", result);

    let empty_order = Oba([0, 0]);

    let mut book = OrderBookCollection {
        bid: [empty_order; MAS],
        ask: [empty_order; MAS],
        pa: empty_order,
        pb: empty_order,
    };

    // 3. Если парсинг прошел успешно, переносим данные в массивы фиксированного размера MAS
    if let Some(extracted_data) = result {
        println!("Parsing success! Last Update ID: {}", extracted_data.id);

        // Заполняем массив bid (primary_list)
        for (i, item) in extracted_data.primary_list.into_iter().take(MAS).enumerate() {
            book.bid[i] = item;
        }

        // Заполняем массив ask (secondary_list)
        for (i, item) in extracted_data.secondary_list.into_iter().take(MAS).enumerate() {
            book.ask[i] = item;
        }

        // Теперь ваш стакан `book` полностью заполнен данными из сети!
        // Можно вызывать ваши методы расчета спреда:
        println!("Spread: {}", book.spread());
    } else {
        eprintln!("Failed to parse orderbook segments from JSON");
    }

    // Заполняем bids (покупка)
    // book.bid[0] = Oba([77953, 225993]);
    // book.bid[1] = Oba([77950, 14]);
    // book.bid[2] = Oba([77950, 41392]);

    // Заполняем asks (продажа)
    // book.ask[0] = Oba([77971, 641]);
    // book.ask[1] = Oba([77974, 0]);
    // book.ask[2] = Oba([77979, 321]);

    // Эти методы изменят внутренние поля book.pa и book.pb и вернут их
    let best_ask = book.pa();
    let best_bid = book.pb();

    // 6. Считаем спред
    let current_spread = book.spread();

    // 7. Выводим результаты в консоль
    println!("--- Данные стакана ордеров ---");
    println!("Лучший Ask (pa): цена = {}, объем = {}", best_ask.0[0], best_ask.0[1]);
    println!("Лучший Bid (pb): цена = {}, объем = {}", best_bid.0[0], best_bid.0[1]);
    println!("Текущий спред: {}", current_spread);

    // Проверка, что данные также обновились внутри самой структуры
    println!("\nПроверка внутренних полей структуры:");
    println!("book.pa: {:?}", book.pa);
    println!("book.pb: {:?}", book.pb);
}
