/// Модель издержек одного круга (вход + выход). Все величины в б.п. (1 б.п. = 0.01% нотионала),
/// кроме ставок комиссий, которые задаются в процентах и переводятся в б.п. умножением на 100.
#[derive(Clone, Copy, Debug)]
struct CostModel {
    /// Комиссия входа, % (0.05 = taker futures VIP0, 0.02 = maker futures VIP0)
    entry_fee_pct: f64,
    /// Комиссия выхода, %
    exit_fee_pct: f64,
    /// Скидка за оплату в BNB как доля (0.10 = 10% на futures, 0.25 = 25% на споте), 0.0 если нет
    bnb_discount: f64,
    /// Полуспред на входе, б.п. (0.0 если вход лимитным ордером - спред не пересекаем)
    half_spread_entry_bps: f64,
    /// Полуспред на выходе, б.п.
    half_spread_exit_bps: f64,
    /// Проскальзывание на входе, б.п. (0.0 для лимитного входа)
    slippage_entry_bps: f64,
    /// Проскальзывание на выходе, б.п.
    slippage_exit_bps: f64,
    /// Ставка funding за 8 часов, б.п. (типично ~1; здесь всегда считается издержкой - консервативно)
    funding_bps_per_8h: f64,
    /// Время удержания позиции, минут
    hold_minutes: f64,
}

impl CostModel {
    /// Полные издержки круга в б.п.:
    /// c_rt = (fee_in% + fee_out%) * 100 * (1 - bnb_discount)   // комиссии, % -> б.п.
    ///      + half_spread_in + half_spread_out                    // пересечение спреда
    ///      + slip_in + slip_out                                  // проскальзывание
    ///      + funding_bps_per_8h * hold_minutes / 480             // funding за время удержания (480 мин = 8 ч)
    fn round_trip_bps(&self) -> f64 {
        let fees = (self.entry_fee_pct + self.exit_fee_pct) * 100.0 * (1.0 - self.bnb_discount);
        let spread = self.half_spread_entry_bps + self.half_spread_exit_bps;
        let slippage = self.slippage_entry_bps + self.slippage_exit_bps;
        let funding = self.funding_bps_per_8h * self.hold_minutes / 480.0;
        fees + spread + slippage + funding
    }
}

/// Безубыточная вероятность касания TP при симметричных барьерах TP = SL = b (gross, б.п.)
/// и издержках c: p*·(b - c) = (1 - p*)·(b + c)  =>  p* = (b + c) / (2b) = 0.5 + c / (2b).
/// Таймауты не учитываются.
fn breakeven_tp_probability(barrier_bps: f64, cost_bps: f64) -> f64 {
    0.5 + cost_bps / (2.0 * barrier_bps)
}

/// Нужный Spearman IC для безубыточности в верхнем дециле сигнала:
/// IC_need = c / (sigma_h * 1.755), где 1.755 - среднее z верхних 10% нормального распределения,
/// sigma_h - стандартное отклонение доходности за горизонт H, б.п.
fn required_ic(cost_bps: f64, sigma_h_bps: f64) -> f64 {
    cost_bps / (sigma_h_bps * 1.755)
}

/// Печатает издержки по сценариям и таблицы безубыточности. Допущения (спред, проскальзывание,
/// funding) замените на измеренные для вашего инструмента и размера позиции.
fn print_cost_scenarios() {
    // Общие допущения для BTCUSDT, 60-минутное удержание
    let base = CostModel {
        entry_fee_pct: 0.05,
        exit_fee_pct: 0.05,
        bnb_discount: 0.0,
        half_spread_entry_bps: 0.05,
        half_spread_exit_bps: 0.05,
        slippage_entry_bps: 0.3,
        slippage_exit_bps: 0.3,
        funding_bps_per_8h: 1.0,
        hold_minutes: 60.0,
    };
    // Сценарии: (название, модель)
    let scenarios = [
        ("futures taker/taker", base),
        (
            "futures taker/taker + BNB",
            CostModel {
                bnb_discount: 0.10,
                ..base
            },
        ),
        // Лимитный вход: комиссия maker, спред и проскальзывание на входе не платим
        (
            "futures maker/taker",
            CostModel {
                entry_fee_pct: 0.02,
                half_spread_entry_bps: 0.0,
                slippage_entry_bps: 0.0,
                ..base
            },
        ),
        // Спот: 0.10% за сторону
        (
            "spot taker/taker",
            CostModel {
                entry_fee_pct: 0.10,
                exit_fee_pct: 0.10,
                ..base
            },
        ),
    ];
    for (name, model) in scenarios.iter() {
        let c = model.round_trip_bps();
        println!("{name:<28} c_rt = {c:.2} bps");
        // Безубыточная вероятность TP для барьеров 29 / 60 / 100 б.п.
        for b in [29.0, 60.0, 100.0] {
            println!(
                "    барьер {b:>5.0} bps: p* = {:.1}%",
                100.0 * breakeven_tp_probability(b, c)
            );
        }
    }
    // Нужный IC при σ_H из вашего скана (FIT): H = 15, 60, 240, 720
    let sigmas = [(15, 17.9), (60, 35.9), (240, 73.5), (720, 133.8)];
    let c = base.round_trip_bps();
    for (h, s) in sigmas {
        println!(
            "H={h:>3} мин, σ={s:>5.1} bps: нужный IC = {:.3} при c = {c:.1} bps",
            required_ic(c, s)
        );
    }
}
