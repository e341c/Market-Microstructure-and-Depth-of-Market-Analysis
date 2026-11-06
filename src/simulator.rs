use std::collections::VecDeque;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OrderSide {
    Buy,
    Sell,
}

#[derive(Debug, Clone)]
pub struct FakeOrder {
    pub id: u64,
    pub side: OrderSide,
    pub price: u64,
    pub volume: u64,
    pub entry_price: u64, // Цена, по которой ордер зашел в стакан
}

pub struct HftSimulator {
    last_update_id: u64,
    active_orders: Vec<FakeOrder>,
    order_id_counter: u64,
    history: VecDeque<(u64, u64, i32)>,

    // Статистика торговой стратегии
    pub total_trades: u32,
    pub total_p_and_l_raw: i64, // Общий P&L в неделимых единицах (целое число)
    pub scale_factor: u64,      // Масштаб цен (например, 10^8 для BTC/USDT)
}

impl HftSimulator {
    pub fn new(scale_factor: u64) -> Self {
        Self {
            last_update_id: 0,
            active_orders: Vec::new(),
            order_id_counter: 0,
            history: VecDeque::with_capacity(100),
            total_trades: 0,
            total_p_and_l_raw: 0,
            scale_factor,
        }
    }

    /// Вспомогательная функция для генерации полного человекочитаемого Timestamp
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

    /// 1. ОБРАБОТКА НОВОГО СОСТОЯНИЯ РЫНКА + ЗАМЕР LATENCY
    pub fn update_market_state(
        &mut self,
        current_id: u64,
        microprice: u64,
        obi: i32,
        best_ask: u64,
        best_bid: u64,
        packet_received_at: Instant, // Время, когда WebSocket прочитал фрейм
    ) {
        let ts = Self::get_full_timestamp();

        // Если пришел битый/пустой пакет из-за ошибки парсинга JSON
        if current_id == 0 && best_ask == 0 && best_bid == 0 {
            println!(
                "[{}] [WARN] Игнорируем некорректный маркет-дата пакет (ID: 0).",
                ts
            );
            return;
        }

        // Проверка хронологии логов
        if current_id <= self.last_update_id {
            println!(
                "[{}] [WARN] Нарушение хронологии пакетов! Отброшен ID: {}, Прошлый: {}",
                ts, current_id, self.last_update_id
            );
            return;
        }

        self.last_update_id = current_id;

        if self.history.len() >= 100 {
            self.history.pop_front();
        }
        self.history.push_back((current_id, microprice, obi));

        // Проверяем сведение ордеров
        self.check_matching(best_ask, best_bid);

        // Принимаем торговое решение
        self.make_predictions_and_trade(microprice, obi, best_bid, best_ask);

        // Считаем задержку обработки (Latency) в микросекундах
        let latency = packet_received_at.elapsed().as_micros();
        println!(
            "[{}] [METRIC] Пакет ID {} обработан за {} μs (микросекунд)",
            ts, current_id, latency
        );
    }

    /// 2. ИСПЛЕНИЕ ОРДЕРОВ И РАСЧЕТ P&L ПРИ СВЕДЕНИИ С РЫНКОМ
    fn check_matching(&mut self, best_ask: u64, best_bid: u64) {
        let mut i = 0;
        while i < self.active_orders.len() {
            let order = &self.active_orders[i];
            let mut executed = false;
            let mut execution_price = 0;

            match order.side {
                OrderSide::Buy => {
                    if best_ask <= order.price {
                        execution_price = best_ask;
                        executed = true;
                    }
                }
                OrderSide::Sell => {
                    if best_bid >= order.price {
                        execution_price = best_bid;
                        executed = true;
                    }
                }
            }

            if executed {
                let ts = Self::get_full_timestamp();
                self.total_trades += 1;

                // Расчет P&L (Прибыль / Убыток)
                // Для BUY: заработали, если цена исполнения ниже, чем планировали продать (эмуляция закрытия)
                // Для простоты симулятора: считаем P&L как разницу между ценой входа (entry) и ценой фактического исполнения
                let p_and_l_per_unit = match order.side {
                    OrderSide::Buy => (order.entry_price as i64) - (execution_price as i64),
                    OrderSide::Sell => (execution_price as i64) - (order.entry_price as i64),
                };

                // Умножаем на объем (перевод в базовый размер позиции, например 1 лот = 1 BTC = 10^8 единиц объема)
                // Если объем равен 1.0 BTC, то в "лампортах" это self.scale_factor
                let trade_p_and_l =
                    (p_and_l_per_unit * (order.volume as i64)) / (self.scale_factor as i64);
                self.total_p_and_l_raw += trade_p_and_l;

                let human_p_and_l = trade_p_and_l as f64 / (self.scale_factor as f64);

                println!(
                    "[{}] [ORDER EXECUTED] Ордер #{:?} {:?} исполнен по цене {}. P&L сделки: {:.4} USDT. Общий накопленный P&L: {:.4} USDT",
                    ts,
                    order.id,
                    order.side,
                    execution_price,
                    human_p_and_l,
                    (self.total_p_and_l_raw as f64 / self.scale_factor as f64)
                );

                self.active_orders.remove(i);
            } else {
                i += 1;
            }
        }
    }

    /// 3. ТОРГОВЫЕ СИГНАЛЫ (ВЫСТАВЛЕНИЕ ФЕЙК-ОРДЕРОВ)
    fn make_predictions_and_trade(
        &mut self,
        microprice: u64,
        obi: i32,
        best_bid: u64,
        best_ask: u64,
    ) {
        let ts = Self::get_full_timestamp();

        // Сигнал на BUY (Покупатели выталкивают стакан вверх)
        if obi > 8000 && microprice >= (best_ask - 200) && self.active_orders.is_empty() {
            self.order_id_counter += 1;
            let order = FakeOrder {
                id: self.order_id_counter,
                side: OrderSide::Buy,
                price: best_bid,           // Выставляем лимитник на лучший бид
                volume: self.scale_factor, // Позиция размером в 1.0 лот
                entry_price: best_bid,
            };
            println!(
                "[{}] [ORDER PLACED] ID: {}, Сигнал: OBI ({}) Давление Покупателей. Выставлен BUY по цене {}",
                ts, order.id, obi, order.price
            );
            self.active_orders.push(order);
        }

        // Сигнал на SELL (Продавцы топят стакан вниз)
        if obi < -8000 && microprice <= (best_bid + 200) && self.active_orders.is_empty() {
            self.order_id_counter += 1;
            let order = FakeOrder {
                id: self.order_id_counter,
                side: OrderSide::Sell,
                price: best_ask,
                volume: self.scale_factor,
                entry_price: best_ask,
            };
            println!(
                "[{}] [ORDER PLACED] ID: {}, Сигнал: OBI ({}) Давление Продавцов. Выставлен SELL по цене {}",
                ts, order.id, obi, order.price
            );
            self.active_orders.push(order);
        }
    }
}
