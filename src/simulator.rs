//! Paper-trading HFT simulator on top of the integer order book.
//! Prices / volumes are u128 scaled by SCALE (1e8); P&L is i128 in the same scale. No floats.

use crate::book::Top;
use crate::fixed::{SCALE, fmt_fixed, fmt_fixed_signed};
use crate::metrics;
use crate::timeutil::utc_timestamp;
use std::collections::VecDeque;
use std::time::Instant;

/// All tunables in one place (ready for optimisation / backtests).
#[derive(Debug, Clone)]
pub struct Params {
    // limit order lifecycle
    pub order_timeout_ms: u128,
    pub min_order_age_ms: u128, // OBI cancel allowed only after this age (anti-flicker)
    // position lifecycle
    pub stop_loss: u128,   // price distance, scaled
    pub take_profit: u128, // price distance, scaled
    pub max_hold_ms: u128, // forced exit by time
    pub min_hold_ms: u128, // OBI-reversal exit allowed only after this age
    // OBI thresholds, scaled by 10_000
    pub obi_maker: i32,   // place maker order above this |OBI|
    pub obi_impulse: i32, // taker entry above this |OBI| (if enabled)
    pub obi_cancel: i32,  // cancel resting order if OBI flips beyond this
    pub obi_exit: i32,    // close position if OBI flips beyond this
    pub taker_impulse: bool,
    // sizing / fees
    pub trade_volume: u128,
    pub taker_fee_ppm: i128,
    pub maker_fee_ppm: i128,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            order_timeout_ms: 10_000,
            min_order_age_ms: 500,
            stop_loss: 40 * SCALE,
            take_profit: 40 * SCALE,
            max_hold_ms: 30_000,
            min_hold_ms: 1_000,
            obi_maker: 6000,
            obi_impulse: 9500,
            obi_cancel: 4000,
            obi_exit: 4000,
            taker_impulse: false,
            trade_volume: SCALE, // 1.0 BTC
            taker_fee_ppm: 200,  // 0.02%
            maker_fee_ppm: 50,   // 0.005%
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct Stats {
    pub orders_placed: u32,
    pub orders_filled: u32,
    pub canceled_timeout: u32,
    pub canceled_obi: u32,
    pub wins: u32,
    pub losses: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OrderSide {
    Buy,
    Sell,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OrderType {
    Limit,
    Market, // taker
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PositionSide {
    None,
    Long,
    Short,
}

#[derive(Debug, Clone)]
pub struct FakeOrder {
    pub id: u64,
    pub side: OrderSide,
    pub order_type: OrderType,
    pub price: u128,
    pub volume: u128,
    pub created_at_id: u64,
    pub placed_at_instant: Instant,
}

pub struct HftSimulator {
    p: Params,
    pub stats: Stats,
    last_update_id: u64,
    active_orders: Vec<FakeOrder>,
    order_id_counter: u64,
    history: VecDeque<(u64, u128, i32)>, // (id, microprice, obi)

    pub current_position: PositionSide,
    pub position_entry_price: u128,
    pub position_volume: u128,
    pub position_entry_type: OrderType,
    position_opened_at: Instant,

    pub total_trades: u32,
    pub total_p_and_l_raw: i128,
}

impl HftSimulator {
    pub fn new() -> Self {
        Self::with_params(Params::default())
    }

    pub fn with_params(p: Params) -> Self {
        Self {
            p,
            stats: Stats::default(),
            last_update_id: 0,
            active_orders: Vec::new(),
            order_id_counter: 0,
            history: VecDeque::with_capacity(100),
            current_position: PositionSide::None,
            position_entry_price: 0,
            position_volume: 0,
            position_entry_type: OrderType::Limit,
            position_opened_at: Instant::now(),
            total_trades: 0,
            total_p_and_l_raw: 0,
        }
    }

    /// Feed one order book update. `current_id` must grow monotonically (FIX MsgSeqNum, tag 34).
    pub fn update_market_state(&mut self, current_id: u64, top: &Top, packet_received_at: Instant) {
        let ts = utc_timestamp();

        let best_bid = top.pb.price() as u128;
        let best_ask = top.pa.price() as u128;
        if best_bid == 0 || best_ask == 0 {
            println!("[{}] [WARN] Пустой топ стакана, пакет пропущен.", ts);
            return;
        }

        if current_id <= self.last_update_id {
            println!(
                "[{}] [WARN] Нарушение хронологии пакетов! Отброшен ID: {}, Прошлый: {}",
                ts, current_id, self.last_update_id
            );
            return;
        }
        self.last_update_id = current_id;

        let microprice = metrics::microprice(top) as u128;
        let obi = metrics::obi(top) as i32;

        if self.history.len() >= 100 {
            self.history.pop_front();
        }
        self.history.push_back((current_id, microprice, obi));

        self.check_exits(best_ask, best_bid, obi);
        self.check_timeouts_and_obi_cancels(obi);
        self.check_matching(best_ask, best_bid);
        self.make_predictions_and_trade(current_id, obi, best_bid, best_ask);

        let _latency = packet_received_at.elapsed().as_micros();
        // DO NOT REMOVE THIS println
        // println!(
        //     "[{}] [METRIC] Пакет ID {} обработан за {} μs (микросекунд)",
        //     ts, current_id, _latency
        // );
    }

    /// Closes the position (market) on stop-loss, take-profit, max hold time or OBI reversal.
    fn check_exits(&mut self, best_ask: u128, best_bid: u128, obi: i32) {
        let entry = self.position_entry_price;
        let (exit_side, exit_price, sl, tp, against) = match self.current_position {
            PositionSide::Long => (
                OrderSide::Sell,
                best_bid,
                best_bid <= entry.saturating_sub(self.p.stop_loss),
                best_bid >= entry.saturating_add(self.p.take_profit),
                obi < -self.p.obi_exit,
            ),
            PositionSide::Short => (
                OrderSide::Buy,
                best_ask,
                best_ask >= entry.saturating_add(self.p.stop_loss),
                best_ask <= entry.saturating_sub(self.p.take_profit),
                obi > self.p.obi_exit,
            ),
            PositionSide::None => return,
        };

        let held = self.position_opened_at.elapsed().as_millis();
        let reason = if sl {
            "STOP-LOSS"
        } else if tp {
            "TAKE-PROFIT"
        } else if held >= self.p.max_hold_ms {
            "MAX-HOLD"
        } else if held >= self.p.min_hold_ms && against {
            "OBI-REVERSAL"
        } else {
            return;
        };

        println!(
            "[{}] [EXIT] {:?}: {} (держим {} ms)",
            utc_timestamp(),
            self.current_position,
            reason,
            held
        );
        let vol = self.position_volume;
        self.process_position_and_p_and_l(exit_side, exit_price, vol, OrderType::Market);
    }

    /// Hard timeout + OBI reversal cancel (only for orders older than min_order_age_ms).
    fn check_timeouts_and_obi_cancels(&mut self, current_obi: i32) {
        let ts = utc_timestamp();
        let mut i = 0;

        while i < self.active_orders.len() {
            let order = &self.active_orders[i];
            let age = order.placed_at_instant.elapsed().as_millis();
            let mut cancel: Option<(String, bool)> = None; // (reason, is_timeout)

            if age >= self.p.order_timeout_ms {
                cancel = Some((format!("Timeout (>{}ms)", self.p.order_timeout_ms), true));
            } else if age >= self.p.min_order_age_ms {
                if order.side == OrderSide::Sell && current_obi > self.p.obi_cancel {
                    cancel = Some(("OBI развернулся против позиции продавца".into(), false));
                } else if order.side == OrderSide::Buy && current_obi < -self.p.obi_cancel {
                    cancel = Some(("OBI развернулся против позиции покупателя".into(), false));
                }
            }

            if let Some((reason, is_timeout)) = cancel {
                println!(
                    "[{}] [ORDER CANCELED] Лимитный ордер #{} {:?} отменен. Причина: {}",
                    ts, order.id, order.side, reason
                );
                if is_timeout {
                    self.stats.canceled_timeout += 1;
                } else {
                    self.stats.canceled_obi += 1;
                }
                self.active_orders.remove(i);
            } else {
                i += 1;
            }
        }
    }

    /// Fill model for resting limit orders (filled at the order's own price):
    ///  - Buy  fills when the market crosses or trades THROUGH our level:
    ///         best_ask <= price  or  best_bid < price (our level is gone downwards).
    ///  - Sell mirrors it: best_bid >= price  or  best_ask > price.
    fn check_matching(&mut self, best_ask: u128, best_bid: u128) {
        let mut i = 0;
        while i < self.active_orders.len() {
            let order = &self.active_orders[i];

            if order.order_type == OrderType::Market {
                i += 1;
                continue;
            }

            let filled = match order.side {
                OrderSide::Buy => best_ask <= order.price || best_bid < order.price,
                OrderSide::Sell => best_bid >= order.price || best_ask > order.price,
            };

            if filled {
                let (id, side, price, volume) = (order.id, order.side, order.price, order.volume);
                println!(
                    "[{}] [ORDER FILLED] #{} {:?} по {}",
                    utc_timestamp(),
                    id,
                    side,
                    fmt_fixed(price)
                );
                self.stats.orders_filled += 1;
                self.active_orders.remove(i);
                self.process_position_and_p_and_l(side, price, volume, OrderType::Limit);
            } else {
                i += 1;
            }
        }
    }

    fn process_position_and_p_and_l(
        &mut self,
        side: OrderSide,
        execution_price: u128,
        volume: u128,
        execution_type: OrderType,
    ) {
        let ts = utc_timestamp();

        let turnover = (execution_price * volume / SCALE) as i128;
        let fee_rate = match execution_type {
            OrderType::Limit => self.p.maker_fee_ppm,
            OrderType::Market => self.p.taker_fee_ppm,
        };
        let fee_raw = turnover * fee_rate / 1_000_000;

        match self.current_position {
            PositionSide::None => {
                self.current_position = match side {
                    OrderSide::Buy => PositionSide::Long,
                    OrderSide::Sell => PositionSide::Short,
                };
                self.position_entry_price = execution_price;
                self.position_volume = volume;
                self.position_entry_type = execution_type;
                self.position_opened_at = Instant::now();
                self.total_p_and_l_raw -= fee_raw;
                println!(
                    "[{}] [POSITION OPENED] Открыта позиция {:?} ({:?}) по цене {}. Объем: {}. Комиссия: {} USDT",
                    ts,
                    self.current_position,
                    execution_type,
                    fmt_fixed(execution_price),
                    fmt_fixed(volume),
                    fmt_fixed_signed(fee_raw),
                );
            }
            PositionSide::Long if side == OrderSide::Sell => {
                let per_unit = execution_price as i128 - self.position_entry_price as i128;
                self.close_position("Лонг", per_unit, volume, fee_raw, execution_price, &ts);
            }
            PositionSide::Short if side == OrderSide::Buy => {
                let per_unit = self.position_entry_price as i128 - execution_price as i128;
                self.close_position("Шорт", per_unit, volume, fee_raw, execution_price, &ts);
            }
            _ => {}
        }
    }

    fn close_position(
        &mut self,
        label: &str,
        per_unit: i128,
        volume: u128,
        fee_raw: i128,
        execution_price: u128,
        ts: &str,
    ) {
        self.total_trades += 1;
        let dirty = per_unit * volume as i128 / SCALE as i128;
        let net = dirty - fee_raw;
        self.total_p_and_l_raw += net;
        if net > 0 {
            self.stats.wins += 1;
        } else {
            self.stats.losses += 1;
        }
        println!(
            "[{}] [POSITION CLOSED] {} закрыт по цене {}. Чистый P&L: {} USDT. Общий P&L: {} USDT",
            ts,
            label,
            fmt_fixed(execution_price),
            fmt_fixed_signed(net),
            fmt_fixed_signed(self.total_p_and_l_raw),
        );
        self.current_position = PositionSide::None;
    }

    fn make_predictions_and_trade(
        &mut self,
        current_id: u64,
        obi: i32,
        best_bid: u128,
        best_ask: u128,
    ) {
        if self.current_position != PositionSide::None {
            return;
        }
        let ts = utc_timestamp();
        let vol = self.p.trade_volume;

        // Optional taker entry on extreme imbalance (disabled by default: pays spread + taker fee).
        if self.p.taker_impulse && self.active_orders.is_empty() {
            if obi > self.p.obi_impulse {
                println!(
                    "[{}] [TAKER IMPULSE] BUY по аску: {}",
                    ts,
                    fmt_fixed(best_ask)
                );
                self.process_position_and_p_and_l(OrderSide::Buy, best_ask, vol, OrderType::Market);
                return;
            }
            if obi < -self.p.obi_impulse {
                println!(
                    "[{}] [TAKER IMPULSE] SELL по биду: {}",
                    ts,
                    fmt_fixed(best_bid)
                );
                self.process_position_and_p_and_l(
                    OrderSide::Sell,
                    best_bid,
                    vol,
                    OrderType::Market,
                );
                return;
            }
        }

        let in_maker_zone = (obi > self.p.obi_maker && obi <= self.p.obi_impulse)
            || (obi < -self.p.obi_maker && obi >= -self.p.obi_impulse);
        if self.active_orders.is_empty() && in_maker_zone {
            self.order_id_counter += 1;
            let side = if obi > 0 {
                OrderSide::Buy
            } else {
                OrderSide::Sell
            };
            let price = if obi > 0 { best_bid } else { best_ask };
            let order = FakeOrder {
                id: self.order_id_counter,
                side,
                order_type: OrderType::Limit,
                price,
                volume: vol,
                created_at_id: current_id,
                placed_at_instant: Instant::now(),
            };
            println!(
                "[{}] [MAKER LIMIT PLACED] ID: {}, Сигнал OBI ({}). Цена: {}",
                ts,
                order.id,
                obi,
                fmt_fixed(order.price)
            );
            self.stats.orders_placed += 1;
            self.active_orders.push(order);
        }
    }

    /// Call periodically (e.g. every 60 s) to see fill rate and P&L.
    pub fn print_summary(&self) {
        let s = &self.stats;
        let fill_bp = if s.orders_placed > 0 {
            s.orders_filled as u64 * 10_000 / s.orders_placed as u64
        } else {
            0
        };
        println!(
            "[{}] [STATS] ордера: выставлено {}, исполнено {} ({}.{:02}%), отмен: таймаут {}, по OBI {} | сделок {} (плюс {}, минус {}) | Общий P&L: {} USDT",
            utc_timestamp(),
            s.orders_placed,
            s.orders_filled,
            fill_bp / 100,
            fill_bp % 100,
            s.canceled_timeout,
            s.canceled_obi,
            self.total_trades,
            s.wins,
            s.losses,
            fmt_fixed_signed(self.total_p_and_l_raw),
        );
    }
}
