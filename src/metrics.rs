//! Microstructure metrics, integer math only (u128 / i128 intermediates).

use crate::book::Top;

pub const OBI_SCALE: i128 = 10_000;

/// Best ask - best bid, scaled by 1e8. 0 if the book is empty or crossed.
pub fn spread(t: &Top) -> u128 {
    if t.pa.price() > 0 && t.pb.price() > 0 && t.pa.price() > t.pb.price() {
        t.pa.price() - t.pb.price()
    } else {
        0
    }
}

/// Microprice = (Pa·Vb + Pb·Va) / (Vb + Va), scaled by 1e8.
pub fn microprice(t: &Top) -> u128 {
    let (pa, va) = (t.pa.price() as u128, t.pa.qty() as u128);
    let (pb, vb) = (t.pb.price() as u128, t.pb.qty() as u128);
    let total = va + vb;
    if total == 0 || pa == 0 || pb == 0 {
        return 0;
    }
    ((pa * vb + pb * va) / total) as u128
}

/// OBI = (Vb − Va) / (Vb + Va), scaled to [-10000, +10000].
pub fn obi(t: &Top) -> i32 {
    let va = t.pa.qty() as i128;
    let vb = t.pb.qty() as i128;
    let total = vb + va;
    if total == 0 {
        return 0;
    }
    ((vb - va) * OBI_SCALE / total) as i32
}

/// Order Flow Imbalance (OFI) = ΔBid - ΔAsk.
/// Возвращает чистое изменение объема ликвидности. Положительное значение — бычий дисбаланс, отрицательное — медвежий.
pub fn ofi(current: &Top, prev: &Top) -> i128 {
    let p_bid_cur = current.pb.price() as i128;
    let v_bid_cur = current.pb.qty() as i128;
    let p_bid_prev = prev.pb.price() as i128;
    let v_bid_prev = prev.pb.qty() as i128;

    let p_ask_cur = current.pa.price() as i128;
    let v_ask_cur = current.pa.qty() as i128;
    let p_ask_prev = prev.pa.price() as i128;
    let v_ask_prev = prev.pa.qty() as i128;

    // 1. Считаем ΔBid (изменение на стороне спроса)
    let delta_bid = if p_bid_cur > p_bid_prev {
        v_bid_cur
    } else if p_bid_cur == p_bid_prev {
        v_bid_cur - v_bid_prev
    } else {
        -v_bid_prev
    };

    // 2. Считаем ΔAsk (изменение на стороне предложения)
    let delta_ask = if p_ask_cur < p_ask_prev {
        -v_ask_cur
    } else if p_ask_cur == p_ask_prev {
        v_ask_cur - v_ask_prev
    } else {
        v_ask_prev
    };

    // OFI = ΔBid - ΔAsk
    delta_bid - delta_ask
}
