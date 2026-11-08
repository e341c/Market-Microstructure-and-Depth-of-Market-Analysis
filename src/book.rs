//! Integer-only order book built from FIX W (snapshot) / X (incremental) messages.
//! Prices and quantities are u128 scaled by 1e8 (see fixed.rs).

use crate::fix::message::Field;
use crate::fixed::parse_fixed;
use crate::parser::FromPair;
use std::collections::BTreeMap;

/// [price, qty], both scaled by 1e8 (same layout as the old Oba).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Oba(pub [u128; 2]);

impl Oba {
    #[inline]
    pub const fn empty() -> Self {
        Self([0, 0])
    }
    #[inline]
    pub fn price(&self) -> u128 {
        self.0[0]
    }
    #[inline]
    pub fn qty(&self) -> u128 {
        self.0[1]
    }
}

/// Best bid (pb) and best ask (pa).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Top {
    pub pb: Oba,
    pub pa: Oba,
}

#[derive(Default)]
pub struct Book {
    bids: BTreeMap<u128, u128>, // price -> qty
    asks: BTreeMap<u128, u128>,
}

#[derive(Default)]
struct Entry {
    action: u8, // 279: '0' new, '1' change, '2' delete
    side: u8,   // 269: '0' bid, '1' offer
    price: u128,
    qty: u128,
    open: bool,
}

impl Book {
    fn apply_entry(&mut self, e: &Entry) {
        if !e.open || e.price == 0 {
            return;
        }
        let side = if e.side == b'0' {
            &mut self.bids
        } else {
            &mut self.asks
        };
        if e.action == b'2' || e.qty == 0 {
            side.remove(&e.price);
        } else {
            side.insert(e.price, e.qty);
        }
    }

    /// Applies a snapshot (W) or incremental update (X).
    pub fn apply(&mut self, msg: &[Field], snapshot: bool) {
        if snapshot {
            self.bids.clear();
            self.asks.clear();
        }
        let mut cur = Entry::default();

        for (tag, v) in msg {
            match tag {
                279 => {
                    self.apply_entry(&cur);
                    cur = Entry {
                        action: v.as_bytes().first().copied().unwrap_or(b'0'),
                        open: true,
                        ..Default::default()
                    };
                }
                269 => {
                    if snapshot {
                        // in snapshots each entry starts with 269
                        self.apply_entry(&cur);
                        cur = Entry {
                            action: b'0',
                            open: true,
                            ..Default::default()
                        };
                    }
                    cur.side = v.as_bytes().first().copied().unwrap_or(b'0');
                }
                270 => cur.price = parse_fixed(v).unwrap_or(0),
                271 => cur.qty = parse_fixed(v).unwrap_or(0),
                _ => {}
            }
        }
        self.apply_entry(&cur);
    }

    /// Best bid / best ask. O(log n) on BTreeMap, no scanning.
    pub fn top(&self) -> Option<Top> {
        let (bp, bv) = self.bids.iter().next_back()?;
        let (ap, av) = self.asks.iter().next()?;
        Some(Top {
            pb: Oba([*bp, *bv]),
            pa: Oba([*ap, *av]),
        })
    }

    /// Replaces the book with a full top-N snapshot (WS partial depth).
    pub fn apply_levels(&mut self, bids: &[Oba], asks: &[Oba]) {
        self.bids.clear();
        self.asks.clear();
        for l in bids {
            if l.price() > 0 && l.qty() > 0 {
                self.bids.insert(l.price(), l.qty());
            }
        }
        for l in asks {
            if l.price() > 0 && l.qty() > 0 {
                self.asks.insert(l.price(), l.qty());
            }
        }
    }
}

impl FromPair for Oba {
    fn from_pair(first: &str, second: &str) -> Option<Self> {
        let price = parse_fixed(first)?;
        let qty = parse_fixed(second)?;
        if price == 0 {
            return None;
        }
        // qty == 0 is allowed: it means "level removed", apply_levels skips it
        Some(Oba([price, qty]))
    }
}
