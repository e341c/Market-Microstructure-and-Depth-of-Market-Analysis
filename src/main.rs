mod orderbook;

use orderbook::{ Oba, OrderBookCollection, MAS };

fn main() {
    let empty_order = Oba([0, 0]);

    let mut book = OrderBookCollection {
        bid: [empty_order; MAS],
        ask: [empty_order; MAS],
        pa: empty_order,
        pb: empty_order,
    };

    // Заполняем bids (покупка)
    book.bid[0] = Oba([77953, 225993]);
    book.bid[1] = Oba([77950, 14]);
    book.bid[2] = Oba([77950, 41392]);

    // Заполняем asks (продажа)
    book.ask[0] = Oba([77971, 641]);
    book.ask[1] = Oba([77974, 0]);
    book.ask[2] = Oba([77979, 321]);

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
