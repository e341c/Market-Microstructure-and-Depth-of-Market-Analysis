// =============================================================================
// Анализ Binance aggTrades: связь объёма, числа исполнений и движения цены.
//
// Cargo.toml (зависимости):
//   plotters = "0.3"
//   csv = "1"
//   (serde отдельно не нужен: csv сам умеет десериализовать строки в кортежи)
//
// Что делает программа:
//   1. читает ./trades/<файл>.csv в память;
//   2. печатает отчёты: корреляции объёма, объём по размеру шага цены,
//      «проход» ордера по книге, движение цены после ордера (с поправкой на режим);
//   3. по флагу DRAW_PLOT рисует график ./plot/metrics.png.
//
// Что изменено относительно присланной версии:
//   - forward_impact: убран старый цикл, который достраивал ордера поверх group_orders
//     (из-за него в таблицах ордера с gap = 0 смешивались с ордерами нужного gap_ms);
//   - TICK вынесен в одну константу, normalize_100 и сборка Row вынесены из main;
//   - убраны неиспользуемые импорты и лишний вызов parent_orders в main;
//   - вызов get_plot включается флагом DRAW_PLOT;
//   - forward_impact печатает заголовок с gap_ms и доли pos/neg/zero;
//   - forward_impact_demeaned печатает число ордеров на каждом шаге.
// =============================================================================

use plotters::prelude::*;
use std::collections::HashMap;
use std::error::Error;
use std::fs;
use std::path::PathBuf;

// ----------------------------------------------------------------------------
// Типы и константы
// ----------------------------------------------------------------------------

// Строка файла aggTrades Binance, по колонкам a,p,q,f,l,T,m,M:
//   .0 a: id агрегированной сделки
//   .1 p: цена
//   .2 q: объём (в BTC)
//   .3 f: id первой исходной сделки внутри агрегата
//   .4 l: id последней исходной сделки внутри агрегата
//   .5 T: время сделки в миллисекундах
//   .6 m: buyer is maker (true: покупатель лимитный, значит агрессор ПРОДАЁТ)
//   .7 M: best match (в расчётах не используется)
type Trade = (u64, f64, f64, u64, u64, u64, bool, bool);

// Строка для графика: (индекс сделки, цена, объём, took, флаг стороны),
// все значения кроме индекса уже нормализованы в диапазон 0..100.
type Row = (usize, f64, f64, f64, f64);

// Склеенный ордер: (суммарный объём, индекс последнего fill в data,
// знак агрессора (+1 покупка, -1 продажа), время последнего fill в мс).
type Order = (f64, usize, f64, u64);

// Минимальный шаг цены BTCUSDT на споте. Нужен, чтобы переводить цену в «тики»:
// тики = разница_цен / TICK. Для другого символа значение надо изменить.
const TICK: f64 = 0.01;

// Рисовать ли график в конце main. График нужен для визуального обзора данных.
const DRAW_PLOT: bool = false;

// ----------------------------------------------------------------------------
// main: порядок работы
// ----------------------------------------------------------------------------

fn main() {
    // Путь к файлу с сделками за один интервал.
    let path = "./trades/1765065600000.csv";

    // Читаем файл в память. Если файла нет или строка не разбирается, паникуем.
    let data: Vec<Trade> = read_trades(path).expect("cannot get data");

    // Нормализованные данные нужны только для графика.
    let data_normal = build_rows(&data);

    // Датасет для модели: признаки по 15-минутным окнам из ВСЕХ файлов папки ./trades,
    // пишется в ./dataset_15m.csv (порог крупного ордера берётся по первым 5 файлам).
    build_dataset_dir("./trades", "./dataset_15m.csv", 5).expect("cannot build dataset");

    // Отчёт 1: корреляции объёма и числа исполнений, объём против движения цены.
    analyse(&data);

    // Отчёт 2: объём по размеру шага цены между соседними сделками.
    qty_by_price_move(&data);

    // Отчёт 3: «проход» ордера по книге внутри самого ордера, по квинтилям объёма.
    impact_report(&data);

    // Отчёт 4: движение цены ПОСЛЕ ордера на горизонтах 100 мс, 1 с, 5 с.
    // gap_ms задаёт, насколько близкие по времени fills одной стороны считаются
    // одним ордером: 0 (только одна и та же миллисекунда), 5 мс, 50 мс.
    for gap in [0u64, 5, 50] {
        forward_impact(&data, &[100, 1000, 5000], gap);
    }

    // Отчёт 5: то же движение, но с вычетом «фона» (среднего по минуте для мелких
    // ордеров), чтобы убрать влияние общего режима рынка.
    for h in [1000u64, 5000] {
        forward_impact_demeaned(&data, 50, h);
    }

    // График по нормализованным данным.
    if DRAW_PLOT {
        get_plot(&data_normal).expect("cannot build plot");
    }
}

// ----------------------------------------------------------------------------
// Чтение данных
// ----------------------------------------------------------------------------

/// Читает CSV с сделками. Первая строка файла (заголовок a,p,q,f,l,T,m,M)
/// пропускается вручную.
fn read_trades(path: &str) -> Result<Vec<Trade>, Box<dyn Error>> {
    // has_headers(false): читатель не пытается сам распознать заголовок и отдаёт
    // все строки как данные. Заголовок отбрасываем ниже по номеру строки.
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(false)
        .from_path(path)?;

    let mut data: Vec<Trade> = Vec::new();

    // deserialize() разбирает каждую строку в кортеж Trade: числа в u64/f64,
    // слова "true"/"false" в bool.
    for (i, result) in rdr.deserialize().enumerate() {
        // Строка 0 это заголовок. Её результат разбора мы не смотрим
        // (текст "a" не превратится в u64, и это не ошибка для нас).
        if i == 0 {
            continue;
        }
        // `?` останавливает чтение, если в строке нельзя разобрать поле.
        let row: Trade = result?;
        data.push(row);
    }
    Ok(data)
}

// ----------------------------------------------------------------------------
// Подготовка данных для графика
// ----------------------------------------------------------------------------

/// Min-max нормализация в диапазон 0..100:
///     x_norm = (x - min) / (max - min) * 100
/// Минимум колонки превращается в 0, максимум в 100. Если все значения равны
/// (max - min ≈ 0), делить нельзя, возвращаем нули.
fn normalize_100(values: &[f64]) -> Vec<f64> {
    // fold с начальным +inf на каждом элементе берёт меньшее: получаем минимум.
    let min = values.iter().copied().fold(f64::INFINITY, f64::min);
    // Аналогично максимум, старт с -inf.
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let range = max - min;

    values
        .iter()
        .map(|v| {
            if range.abs() < f64::EPSILON {
                0.0
            } else {
                (v - min) / range * 100.0
            }
        })
        .collect()
}

/// Собирает из сырых сделок строки для графика: (индекс, цена, объём, took, сторона).
fn build_rows(data: &[Trade]) -> Vec<Row> {
    // Сырые колонки.
    let prices: Vec<f64> = data.iter().map(|r| r.1).collect();
    let quantities: Vec<f64> = data.iter().map(|r| r.2).collect();

    // took = last_id - first_id: сколько исходных сделок склеено в агрегат минус одна.
    // Ноль значит, что агрегат это одна исходная сделка. saturating_sub не паникует,
    // если l < f (даёт 0).
    let trades_took: Vec<f64> = data
        .iter()
        .map(|r| r.4.saturating_sub(r.3) as f64)
        .collect();

    // Каждая колонка отдельно приводится к 0..100.
    let price_n = normalize_100(&prices);
    let quantity_n = normalize_100(&quantities);
    let trades_n = normalize_100(&trades_took);

    let mut rows: Vec<Row> = Vec::with_capacity(data.len());
    for (i, row) in data.iter().enumerate() {
        // Сторона: 100, если buyer is maker, иначе 0 (уже в диапазоне 0..100).
        let buyer_is_maker_normal = if row.6 { 100.0 } else { 0.0 };
        rows.push((
            i,
            price_n[i],
            quantity_n[i],
            trades_n[i],
            buyer_is_maker_normal,
        ));
    }
    rows
}

// ----------------------------------------------------------------------------
// График
// ----------------------------------------------------------------------------

/// Сжимает колонку до `buckets` точек. Для каждой корзины считает:
///     mean = сумма / количество,  min,  max
/// Нужен, потому что миллионы точек на ширине в 1600 пикселей сливаются в заливку.
/// Возвращает (x, mean, min, max), где x это индекс сделки из середины корзины.
fn downsample(data: &[Row], col: &[f64], buckets: usize) -> Vec<(usize, f64, f64, f64)> {
    // Размер корзины в сделках: len / buckets с округлением вверх, минимум 1.
    let chunk = col.len().div_ceil(buckets).max(1);

    col.chunks(chunk) // режем колонку на куски по chunk элементов
        .enumerate()
        .map(|(i, c)| {
            // Минимум, максимум и сумма по куску за один проход.
            let (mut mn, mut mx, mut sum) = (f64::INFINITY, f64::NEG_INFINITY, 0.0);
            for &v in c {
                mn = mn.min(v);
                mx = mx.max(v);
                sum += v;
            }
            // Координата X: индекс сделки из середины куска (берём из data[..].0).
            let x = data[i * chunk + c.len() / 2].0;
            (x, sum / c.len() as f64, mn, mx)
        })
        .collect()
}

/// Рисует тёмный график из 4 панелей (price, quantity, trades took, buyer is maker)
/// в файл ./plot/metrics.png.
fn get_plot(data_normal: &[Row]) -> Result<(), Box<dyn Error>> {
    if data_normal.is_empty() {
        return Err("get_plot: нет данных для графика".into());
    }

    // --- тема: цвета заданы в RGB ---
    let bg = RGBColor(24, 24, 24); // фон картинки
    let grid = RGBColor(60, 60, 60); // линии сетки (чуть светлее фона)
    let axis = RGBColor(90, 90, 90); // линии осей
    let colors = [
        RGBColor(86, 180, 233),  // голубой  (price)
        RGBColor(240, 228, 66),  // жёлтый   (quantity)
        RGBColor(0, 200, 120),   // зелёный  (trades took)
        RGBColor(230, 100, 100), // красный  (buyer is maker)
    ];

    // Создаём папку для картинки, если её ещё нет.
    fs::create_dir_all("./plot")?;

    // Холст 1600x1200 пикселей, запись в PNG. Заливаем фоном.
    let root = BitMapBackend::new("./plot/metrics.png", (1600, 1200)).into_drawing_area();
    root.fill(&bg)?;

    // Четыре серии: название панели и функция, достающая значение из Row.
    let series: [(&str, fn(&Row) -> f64); 4] = [
        ("price", |r| r.1),
        ("quantity", |r| r.2),
        ("trades took", |r| r.3),
        ("buyer is maker (share, %)", |r| r.4),
    ];

    // Границы оси X: минимальный и максимальный индекс сделки.
    // .max(x_min + 1) нужен, чтобы диапазон не оказался пустым.
    let x_min = data_normal.iter().map(|r| r.0).min().unwrap_or(0);
    let x_max = data_normal
        .iter()
        .map(|r| r.0)
        .max()
        .unwrap_or(1)
        .max(x_min + 1);

    // Сколько точек оставляем на каждой панели после сжатия.
    let buckets = 1000;

    // Делим холст на 4 строки по 1 колонке (по панели на серию).
    let panels = root.split_evenly((series.len(), 1));

    for (i, ((name, get), area)) in series.iter().zip(panels.iter()).enumerate() {
        // Достаём колонку для этой серии и сжимаем до buckets точек.
        let col: Vec<f64> = data_normal.iter().map(get).collect();
        let ds = downsample(data_normal, &col, buckets);
        let color = colors[i % colors.len()];

        // Каркас графика внутри панели.
        let mut chart = ChartBuilder::on(area)
            // Заголовок панели: шрифт 22, белый цвет.
            .caption(*name, ("sans-serif", 22).into_font().color(&WHITE))
            .margin(10) // отступ от края панели, пиксели
            .x_label_area_size(30) // высота области с подписями оси X
            .y_label_area_size(55) // ширина области с подписями оси Y
            // Диапазоны осей. Y: данные лежат в 0..100, берём -5..105 для запаса,
            // чтобы линии на краях не прилипали к рамке.
            .build_cartesian_2d(x_min..x_max, -5.0f64..105.0f64)?;

        // Сетка и подписи осей.
        chart
            .configure_mesh()
            .bold_line_style(grid.stroke_width(1)) // основная сетка
            .light_line_style(grid.mix(0.0)) // мелкая сетка с нулевой непрозрачностью (скрыта)
            .axis_style(axis.stroke_width(1)) // линии осей
            .label_style(("sans-serif", 14).into_font().color(&WHITE)) // цифры на осях
            .draw()?;

        // Полоса от min до max в каждой корзине. Контур многоугольника:
        // верхняя кромка (max) слева направо, затем нижняя (min) справа налево.
        let band: Vec<(usize, f64)> = ds
            .iter()
            .map(|d| (d.0, d.3))
            .chain(ds.iter().rev().map(|d| (d.0, d.2)))
            .collect();
        chart.draw_series(std::iter::once(Polygon::new(
            band,
            color.mix(0.25).filled(), // заливка с прозрачностью 25%
        )))?;

        // Линия среднего значения по корзинам, толщина 2.
        chart.draw_series(LineSeries::new(
            ds.iter().map(|d| (d.0, d.1)),
            color.stroke_width(2),
        ))?;
    }

    // Записываем изображение в файл.
    root.present()?;
    Ok(())
}

// ----------------------------------------------------------------------------
// Статистика: корреляции
// ----------------------------------------------------------------------------

/// Коэффициент корреляции Пирсона:
///     r = Σ(x - x̄)(y - ȳ) / sqrt( Σ(x - x̄)² · Σ(y - ȳ)² )
/// Значения от -1 до 1: измеряет ЛИНЕЙНУЮ связь, чувствителен к выбросам.
fn pearson(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len() as f64;
    // Средние значения x̄ и ȳ.
    let ma = a.iter().sum::<f64>() / n;
    let mb = b.iter().sum::<f64>() / n;
    // cov: числитель, va и vb: суммы квадратов отклонений.
    let (mut cov, mut va, mut vb) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        let (dx, dy) = (x - ma, y - mb);
        cov += dx * dy;
        va += dx * dx;
        vb += dy * dy;
    }
    cov / (va.sqrt() * vb.sqrt())
}

/// Ранги значений (1-й по величине = наименьшее значение). Одинаковые значения
/// получают средний ранг группы: если значения на позициях i..=j равны, каждому
/// присваивается (i + j) / 2. Это важно для took, где много нулей.
fn ranks(v: &[f64]) -> Vec<f64> {
    // Индексы, отсортированные по значению (total_cmp корректно ведёт себя с NaN).
    let mut idx: Vec<usize> = (0..v.len()).collect();
    idx.sort_by(|&i, &j| v[i].total_cmp(&v[j]));

    let mut r = vec![0.0; v.len()];
    let mut i = 0;
    while i < idx.len() {
        // Находим конец группы равных значений [i, j].
        let mut j = i;
        while j + 1 < idx.len() && v[idx[j + 1]] == v[idx[i]] {
            j += 1;
        }
        // Средний ранг для всей группы.
        let avg = (i + j) as f64 / 2.0;
        for k in i..=j {
            r[idx[k]] = avg;
        }
        i = j + 1;
    }
    r
}

/// Корреляция Спирмена = Пирсон, посчитанный по рангам.
/// Измеряет МОНОТОННУЮ связь и устойчив к выбросам и тяжёлым хвостам.
fn spearman(a: &[f64], b: &[f64]) -> f64 {
    pearson(&ranks(a), &ranks(b))
}

/// Среднее = сумма / количество, либо None для пустой подвыборки (чтобы не делить на ноль).
fn mean_or_none(sum: f64, n: usize) -> Option<f64> {
    if n == 0 { None } else { Some(sum / n as f64) }
}

/// Процентиль в отсортированном срезе: элемент на позиции floor((len - 1) * p).
/// p = 0.5 даёт медиану, p = 0.99 даёт 99-й процентиль.
fn pct(v: &[f64], p: f64) -> f64 {
    v[((v.len() - 1) as f64 * p) as usize]
}

// ----------------------------------------------------------------------------
// Отчёт 1: объём, число исполнений, движение цены
// ----------------------------------------------------------------------------

fn analyse(data: &[Trade]) {
    // Объём каждой строки.
    let qty: Vec<f64> = data.iter().map(|r| r.2).collect();

    // took = l - f (число склеенных исходных сделок минус одна).
    let took: Vec<f64> = data
        .iter()
        .map(|r| r.4.saturating_sub(r.3) as f64)
        .collect();

    // Логарифмы сглаживают тяжёлые хвосты:
    //   ln(qty): объём всегда > 0, поэтому обычный логарифм;
    //   ln(1 + took): took бывает 0, поэтому ln_1p (иначе ln(0) = -inf).
    let qty_ln: Vec<f64> = qty.iter().map(|v| v.ln()).collect();
    let took_ln: Vec<f64> = took.iter().map(|v| v.ln_1p()).collect();

    // --- 1. объём против числа исполнений ---
    println!("pearson raw      = {:.3}", pearson(&qty, &took));
    println!("pearson ln       = {:.3}", pearson(&qty_ln, &took_ln));
    println!("spearman         = {:.3}", spearman(&qty, &took));

    // --- 2. multi-fill против single-fill ---
    // multi-fill: агрегат из нескольких исходных сделок (l > f);
    // single-fill: ровно из одной (l == f).
    let (mut s_multi, mut n_multi, mut s_single, mut n_single) = (0.0, 0usize, 0.0, 0usize);
    for r in data {
        if r.4 > r.3 {
            s_multi += r.2;
            n_multi += 1;
        } else {
            s_single += r.2;
            n_single += 1;
        }
    }
    // Доля группы = n_группы / n_всего · 100%, плюс средний объём в группе.
    println!(
        "multi-fill : share={:.1}%  mean qty={:?}",
        100.0 * n_multi as f64 / data.len() as f64,
        mean_or_none(s_multi, n_multi)
    );
    println!(
        "single-fill: share={:.1}%  mean qty={:?}",
        100.0 * n_single as f64 / data.len() as f64,
        mean_or_none(s_single, n_single)
    );

    // --- 3. объём против движения цены ---
    // dprice[i] = |p[i+1] - p[i]|: модуль изменения цены между соседними сделками.
    // Его сопоставляем с объёмом и took сделки i+1, поэтому берём срезы с индекса 1.
    let dprice: Vec<f64> = data.windows(2).map(|w| (w[1].1 - w[0].1).abs()).collect();
    let qty_tail = &qty[1..];
    let took_tail = &took[1..];

    println!(
        "spearman(qty,  |dprice|) = {:.3}",
        spearman(qty_tail, &dprice)
    );
    println!(
        "spearman(took, |dprice|) = {:.3}",
        spearman(took_tail, &dprice)
    );

    // Доля пар сделок, где цена не изменилась.
    let zero_moves = dprice.iter().filter(|d| **d == 0.0).count();
    println!(
        "сделок без изменения цены: {:.1}%",
        100.0 * zero_moves as f64 / dprice.len() as f64
    );
}

// ----------------------------------------------------------------------------
// Отчёт 2: объём по размеру шага цены
// ----------------------------------------------------------------------------

fn qty_by_price_move(data: &[Trade]) {
    // Объёмы, разложенные по группам: шаг цены 0, 1, 2, 3+ тиков.
    let mut qty: [Vec<f64>; 4] = Default::default();
    // Сколько раз сторона (m) осталась той же, что у предыдущей сделки, по группам.
    let mut same_side = [0usize; 4];

    for w in data.windows(2) {
        // Шаг цены в тиках: |p[i+1] - p[i]| / TICK, округлённый до целого.
        let ticks = (((w[1].1 - w[0].1).abs()) / TICK).round() as usize;
        // Группа: 0, 1, 2 или 3 (всё что 3 и больше).
        let k = ticks.min(3);
        // Объём берём у сделки i+1, на которой цена сдвинулась.
        qty[k].push(w[1].2);
        // Совпадает ли сторона у двух соседних сделок (отличает движение внутри
        // одного ордера от отскока bid-ask, где сторона меняется).
        if w[1].6 == w[0].6 {
            same_side[k] += 1;
        }
    }

    for (k, v) in qty.iter_mut().enumerate() {
        if v.is_empty() {
            continue;
        }
        // Сортировка нужна для медианы.
        v.sort_by(|a, b| a.total_cmp(b));
        let mean = v.iter().sum::<f64>() / v.len() as f64;
        let median = v[v.len() / 2];
        let label = if k == 3 {
            "3+".to_string()
        } else {
            k.to_string()
        };
        println!(
            "ticks={:>2}  n={:>8}  mean={:.5}  median={:.5}  same-side={:.1}%",
            label,
            v.len(),
            mean,
            median,
            // Доля совпадения стороны = совпавшие / все в группе · 100%.
            100.0 * same_side[k] as f64 / v.len() as f64
        );
    }
}

// ----------------------------------------------------------------------------
// Отчёт 3: «проход» ордера по книге внутри ордера
// ----------------------------------------------------------------------------

/// Один «родительский» ордер: подряд идущие строки с одной стороной и одной миллисекундой.
struct Parent {
    qty: f64,        // суммарный объём
    fills: usize,    // сколько строк aggTrades склеено
    walk_ticks: f64, // проход цены внутри ордера, в тиках, + = в сторону агрессора
    gap_ms: u64,     // пауза между предыдущей сделкой и первой сделкой ордера
}

fn parent_orders(data: &[Trade]) -> Vec<Parent> {
    let mut out = Vec::new();
    // Начинаем с индекса 1: строка 0 нужна только как «предыдущая сделка».
    let mut i = 1;
    while i < data.len() {
        let start = i;
        // Сторона и время первой строки ордера.
        let (m, t) = (data[i].6, data[i].5);
        let mut qty = 0.0;
        // Пока сторона и миллисекунда те же, добавляем объём к ордеру.
        while i < data.len() && data[i].6 == m && data[i].5 == t {
            qty += data[i].2;
            i += 1;
        }
        // Знак агрессора: m = true значит агрессор продаёт (цена идёт вниз),
        // тогда умножаем смещение на -1, чтобы «+» всегда значило «в сторону агрессора».
        let sign = if m { -1.0 } else { 1.0 };
        out.push(Parent {
            qty,
            fills: i - start,
            // walk = (цена_последнего_fill - цена_первого_fill) · sign / TICK
            walk_ticks: (data[i - 1].1 - data[start].1) * sign / TICK,
            // Пауза до ордера = T_первой_строки - T_предыдущей_сделки.
            gap_ms: t.saturating_sub(data[start - 1].5),
        });
    }
    out
}

fn impact_report(data: &[Trade]) {
    let mut p = parent_orders(data);
    // Сортируем по объёму, чтобы поделить на квинтили.
    p.sort_by(|a, b| a.qty.total_cmp(&b.qty));
    let n = p.len();

    // 5 квинтилей: каждый содержит одинаковое число ордеров (по 20%).
    for q in 0..5 {
        let c = &p[q * n / 5..(q + 1) * n / 5];
        // Проход каждого ордера квинтиля, отсортированный для процентилей.
        let mut w: Vec<f64> = c.iter().map(|x| x.walk_ticks).collect();
        w.sort_by(|a, b| a.total_cmp(b));
        println!(
            "Q{} qty<={:.5} walk: mean={:.2} p50={:.1} p90={:.1} p99={:.1} max={:.0}",
            q + 1,
            c.last().unwrap().qty, // верхняя граница объёма квинтиля
            w.iter().sum::<f64>() / w.len() as f64,
            pct(&w, 0.5),
            pct(&w, 0.9),
            pct(&w, 0.99),
            w[w.len() - 1],
        );
    }

    // Десять самых крупных ордеров по объёму: сколько fills, какой проход, какая пауза до них.
    println!("--- топ-10 по объёму ---");
    for x in p.iter().rev().take(10) {
        println!(
            "qty={:.3} fills={} walk={:.0} gap_ms={}",
            x.qty, x.fills, x.walk_ticks, x.gap_ms
        );
    }

    // Спирмен между объёмом и проходом на уровне ордеров.
    let q: Vec<f64> = p.iter().map(|x| x.qty).collect();
    let w: Vec<f64> = p.iter().map(|x| x.walk_ticks).collect();
    println!("spearman(qty, walk) = {:.3}", spearman(&q, &w));
}

// ----------------------------------------------------------------------------
// Склейка ордеров
// ----------------------------------------------------------------------------

/// Склеивает подряд идущие строки одной стороны в ордер. Строка присоединяется
/// к текущему ордеру, если сторона та же и пауза от предыдущей строки не больше gap_ms:
///     data[i].T <= last_t + gap_ms
/// При gap_ms = 0 ордер это строки одной стороны с одной и той же миллисекундой.
fn group_orders(data: &[Trade], gap_ms: u64) -> Vec<Order> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let m = data[i].6; // сторона нового ордера
        let mut qty = 0.0; // суммарный объём ордера
        let mut last_t = data[i].5; // время последней присоединённой строки
        while i < data.len() && data[i].6 == m && data[i].5 <= last_t + gap_ms {
            qty += data[i].2;
            last_t = data[i].5;
            i += 1;
        }
        // Индекс последнего fill = i - 1 (i уже указывает на следующую строку).
        // Знак агрессора: m = true значит агрессор продаёт.
        out.push((qty, i - 1, if m { -1.0 } else { 1.0 }, last_t));
    }
    out
}

// ----------------------------------------------------------------------------
// Отчёт 4: что делает цена после ордера
// ----------------------------------------------------------------------------

fn forward_impact(data: &[Trade], horizons_ms: &[u64], gap_ms: u64) {
    // Склеиваем ордера с заданным gap_ms.
    let orders = group_orders(data, gap_ms);
    println!(
        "=========== gap_ms = {} (ордеров: {}) ===========",
        gap_ms,
        orders.len()
    );

    // Границы корзин по объёму: 80-й, 95-й, 99-й и 99,9-й процентили объёма ордера.
    let mut sorted: Vec<f64> = orders.iter().map(|o| o.0).collect();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let at = |p: f64| sorted[((sorted.len() - 1) as f64 * p) as usize];
    let cuts = [at(0.80), at(0.95), at(0.99), at(0.999)];
    let names = ["0-80%", "80-95%", "95-99%", "99-99.9%", "top 0.1%"];

    for &h in horizons_ms {
        println!(
            "--- горизонт {} мс (в тиках, + = продолжение, - = откат) ---",
            h
        );
        // По вектору смещений на каждую из 5 корзин.
        let mut buckets: Vec<Vec<f64>> = vec![Vec::new(); 5];

        for &(qty, end, sign, t) in &orders {
            // Номер корзины = сколько границ объём ордера превышает (0..4).
            let b = cuts.iter().filter(|&&c| qty > c).count();
            // Индекс первой сделки позже t + h (бинарный поиск: data отсортирована по T).
            let j = data.partition_point(|r| r.5 <= t + h);
            // Если горизонт выходит за конец данных, ордер пропускаем.
            if j == data.len() {
                continue;
            }
            // Смещение = (цена на момент t + h - цена последнего fill ордера) · sign / TICK.
            // data[j - 1] это последняя сделка не позже t + h.
            // Положительное значение: цена пошла в сторону агрессора (продолжение).
            buckets[b].push((data[j - 1].1 - data[end].1) * sign / TICK);
        }

        for (b, v) in buckets.iter_mut().enumerate() {
            if v.is_empty() {
                continue;
            }
            v.sort_by(|a, b| a.total_cmp(b)); // сортировка нужна для медианы
            let n = v.len() as f64;
            let mean = v.iter().sum::<f64>() / n;
            // Доли смещений: > 0 (продолжение), < 0 (откат), = 0 (цена не сдвинулась).
            let pos = 100.0 * v.iter().filter(|x| **x > 0.0).count() as f64 / n;
            let neg = 100.0 * v.iter().filter(|x| **x < 0.0).count() as f64 / n;
            println!(
                "{:<9} n={:>7} mean={:>7.2} median={:>6.1} pos={:.1}% neg={:.1}% zero={:.1}%",
                names[b],
                v.len(),
                mean,
                v[v.len() / 2],
                pos,
                neg,
                100.0 - pos - neg
            );
        }
    }
}

// ----------------------------------------------------------------------------
// Отчёт 5: то же, но с поправкой на режим рынка
// ----------------------------------------------------------------------------

fn forward_impact_demeaned(data: &[Trade], gap_ms: u64, h: u64) {
    let orders = group_orders(data, gap_ms);

    // Для каждого ордера: (объём, номер минуты, смещение цены через h мс в тиках).
    let mut rows: Vec<(f64, u64, f64)> = Vec::with_capacity(orders.len());
    for &(qty, end, sign, t) in &orders {
        let j = data.partition_point(|r| r.5 <= t + h);
        if j == data.len() {
            continue; // горизонт за концом данных
        }
        // Номер минуты = T / 60000 (целочисленное деление).
        rows.push((qty, t / 60_000, (data[j - 1].1 - data[end].1) * sign / TICK));
    }

    // Порог мелкого ордера: 80-й процентиль объёма.
    let mut qs: Vec<f64> = rows.iter().map(|r| r.0).collect();
    qs.sort_by(|a, b| a.total_cmp(b));
    let q80 = qs[((qs.len() - 1) as f64 * 0.80) as usize];

    // Фон режима: среднее смещение МЕЛКИХ ордеров в каждой минуте.
    // (сумма смещений, количество) по минуте.
    let mut by_min: HashMap<u64, (f64, usize)> = HashMap::new();
    for &(q, m, f) in &rows {
        if q > q80 {
            continue; // крупные ордера в фон не входят
        }
        let e = by_min.entry(m).or_insert((0.0, 0));
        e.0 += f;
        e.1 += 1;
    }

    // Избыток над фоном: смещение ордера минус среднее мелких ордеров той же минуты.
    //     excess = f - (сумма / количество)_минуты
    // Минуты без мелких ордеров пропускаются.
    let mut ex: Vec<(f64, f64)> = Vec::with_capacity(rows.len());
    for &(q, m, f) in &rows {
        if let Some(&(s, n)) = by_min.get(&m) {
            ex.push((q, f - s / n as f64));
        }
    }
    // Сортировка по объёму: далее корзины берутся по позиции в этом списке.
    ex.sort_by(|a, b| a.0.total_cmp(&b.0));

    println!(
        "orders={} rows={} ex={}  (ордеров всего / с известным горизонтом / после вычета фона)",
        orders.len(),
        rows.len(),
        ex.len()
    );

    let n = ex.len();
    // Границы корзин как доли списка: 0-80%, 80-95%, 95-99%, 99-99.9%, 99.9-100%.
    let edges = [0.0, 0.80, 0.95, 0.99, 0.999, 1.0];
    let names = ["0-80%", "80-95%", "95-99%", "99-99.9%", "top 0.1%"];

    println!(
        "--- gap {} мс, горизонт {} мс, минус среднее по минуте ---",
        gap_ms, h
    );
    for b in 0..5 {
        // Срез отсортированного по объёму списка, соответствующий корзине.
        let c = &ex[(n as f64 * edges[b]) as usize..(n as f64 * edges[b + 1]) as usize];
        if c.is_empty() {
            continue;
        }
        // Избытки этой корзины, отсортированные для медианы.
        let mut v: Vec<f64> = c.iter().map(|x| x.1).collect();
        v.sort_by(|a, b| a.total_cmp(b));
        println!(
            "{:<9} n={:>7} mean={:>7.2} median={:>6.1}",
            names[b],
            v.len(),
            v.iter().sum::<f64>() / v.len() as f64,
            v[v.len() / 2]
        );
    }
}

// ----------------------------------------------------------------------------
// Признаки по 15-минутным окнам (датасет для модели)
// ----------------------------------------------------------------------------

/// Длина окна в миллисекундах: 15 минут * 60 секунд * 1000 мс.
/// Номер окна для сделки считается как T / WINDOW_MS (целочисленное деление).
const WINDOW_MS: u64 = 15 * 60 * 1000;

/// Признаки одного 15-минутного окна. Считаются только по сделкам внутри окна.
struct Feat {
    t0: u64,         // начало окна в мс (номер окна * WINDOW_MS)
    n_trades: usize, // число строк aggTrades в окне: мера активности
    volume: f64,     // суммарный объём окна в BTC: Σq
    imbalance: f64,  // дисбаланс потока: (buy - sell) / (buy + sell), в диапазоне -1..1
    ret: f64,        // доходность окна в логарифмах: ln(close / open)
    range: f64,      // относительный размах: (high - low) / open
    rv: f64,         // realized variance: Σ ln(p[k+1] / p[k])² по ценам раз в секунду
    big_net: f64,    // чистый объём крупных ордеров: Σ qty * sign (+ покупки, - продажи)
    big_share: f64,  // доля объёма окна, пришедшаяся на крупные ордера
}

/// Считает признаки одного окна.
///   w       - срез сделок, попавших в окно (не пустой, отсортирован по времени);
///   t0      - начало окна в мс;
///   big_qty - порог объёма, выше которого ордер считается крупным.
fn window_features(w: &[Trade], t0: u64, big_qty: f64) -> Feat {
    // Цена открытия и закрытия окна: первая и последняя сделка.
    let open = w[0].1;
    let close = w[w.len() - 1].1;

    // high и low ищем одним проходом: стартуем с самых «плохих» значений.
    let (mut hi, mut lo) = (f64::MIN, f64::MAX);

    // Объём покупок и продаж агрессора.
    let (mut buy, mut sell) = (0.0, 0.0);

    // Последняя цена каждой секунды. Волатильность по каждой сделке зашумлена
    // отскоком между bid и ask, поэтому берём цены раз в секунду.
    let mut sec_prices: Vec<f64> = Vec::new();
    let mut last_sec = u64::MAX; // секунда предыдущей сделки (MAX = «ещё не было»)

    for r in w {
        hi = hi.max(r.1); // обновляем максимум цены
        lo = lo.min(r.1); // обновляем минимум цены

        // m = true: покупатель лимитный, значит агрессор ПРОДАЁТ.
        if r.6 {
            sell += r.2;
        } else {
            buy += r.2;
        }

        // Номер секунды сделки. Если секунда та же, перезаписываем цену последней
        // сделкой секунды, иначе начинаем новую запись.
        let s = r.5 / 1000;
        if s == last_sec {
            *sec_prices.last_mut().unwrap() = r.1;
        } else {
            sec_prices.push(r.1);
            last_sec = s;
        }
    }

    // Realized variance = Σ ln(p[k+1] / p[k])² по соседним секундным ценам.
    // Это оценка дисперсии цены за окно, корень из неё даёт волатильность.
    let rv: f64 = sec_prices
        .windows(2)
        .map(|p| (p[1] / p[0]).ln().powi(2))
        .sum();

    // Крупные ордера: склеиваем fills (пауза до 50 мс) и берём те, у которых
    // объём выше порога. big_net учитывает направление, big_vol только объём.
    let (mut big_net, mut big_vol) = (0.0, 0.0);
    for (qty, _, sign, _) in group_orders(w, 50) {
        if qty > big_qty {
            big_net += qty * sign;
            big_vol += qty;
        }
    }

    let volume = buy + sell;
    Feat {
        t0,
        n_trades: w.len(),
        volume,
        // Если объём ноль, делить нельзя: дисбаланс и доля равны 0.
        imbalance: if volume > 0.0 {
            (buy - sell) / volume
        } else {
            0.0
        },
        ret: (close / open).ln(),
        range: (hi - lo) / open,
        rv,
        big_net,
        big_share: if volume > 0.0 { big_vol / volume } else { 0.0 },
    }
}

/// Допуск на краях окна, мс. Окно считается полным, если первая сделка не позже
/// чем через EDGE_MS после начала окна, а последняя не раньше, чем за EDGE_MS до конца.
/// Нужен, чтобы отбрасывать окна, обрезанные границей файла или дырой в данных.
const EDGE_MS: u64 = 60_000;

/// Проверяет, что сделки окна покрывают его целиком (с допуском EDGE_MS на краях).
///   w  - сделки окна (не пустой срез), t0 - начало окна в мс.
fn is_full_window(w: &[Trade], t0: u64) -> bool {
    let first = w[0].5; // время первой сделки окна
    let last = w[w.len() - 1].5; // время последней сделки окна
    // Сколько мс от начала окна до первой сделки и от последней сделки до конца окна.
    first.saturating_sub(t0) <= EDGE_MS && (t0 + WINDOW_MS).saturating_sub(last) <= EDGE_MS
}

/// Режет сделки на 15-минутные окна и считает признаки каждого ПОЛНОГО окна.
/// Неполные окна (обрезанные границей данных) пропускаются.
/// Ожидает, что data отсортирована по времени.
fn features_15m(data: &[Trade], big_qty: f64) -> Vec<Feat> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        // Номер окна текущей сделки.
        let b = data[i].5 / WINDOW_MS;
        let start = i;
        // Двигаемся вперёд, пока сделки остаются в том же окне.
        while i < data.len() && data[i].5 / WINDOW_MS == b {
            i += 1;
        }
        // Срез [start, i) это все сделки окна b, t0 это начало окна.
        let w = &data[start..i];
        let t0 = b * WINDOW_MS;
        // Неполные окна не берём: их признаки были бы посчитаны по части интервала.
        if is_full_window(w, t0) {
            out.push(window_features(w, t0, big_qty));
        }
    }
    out
}

/// Пишет датасет в CSV: строка = признаки окна t + цели из окна t+1
/// (next_ret: доходность следующего окна, next_rv: его волатильность).
/// Цели берутся из СЛЕДУЮЩЕГО окна, чтобы модель не видела будущее в признаках.
fn write_dataset(feats: &[Feat], path: &str) -> Result<(), Box<dyn Error>> {
    // Первая строка файла: названия колонок.
    let mut s = String::from(
        "t0,n_trades,volume,imbalance,ret,range,rv,big_net,big_share,next_ret,next_rv\n",
    );

    // windows(2) даёт пары соседних окон (a = текущее, b = следующее).
    for w in feats.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        // Если в данных есть дыра, следующее окно не соседнее: такую пару пропускаем,
        // иначе цель относилась бы к другому моменту времени.
        if b.t0 != a.t0 + WINDOW_MS {
            continue;
        }
        s.push_str(&format!(
            "{},{},{:.6},{:.6},{:.8},{:.8},{:.10},{:.6},{:.6},{:.8},{:.10}\n",
            a.t0,
            a.n_trades,
            a.volume,
            a.imbalance,
            a.ret,
            a.range,
            a.rv,
            a.big_net,
            a.big_share,
            b.ret,
            b.rv
        ));
    }
    fs::write(path, s)?;
    Ok(())
}

// ----------------------------------------------------------------------------
// Датасет по всей папке с файлами сделок
// ----------------------------------------------------------------------------

/// Возвращает пути к csv-файлам папки, отсортированные по времени.
/// Имя файла - это timestamp в мс (например 1765065600000.csv), по нему и сортируем.
/// Сортируем как число, а не как строку, чтобы порядок не зависел от длины имени.
fn list_trade_files(dir: &str) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let mut files: Vec<(u64, PathBuf)> = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        // Берём только файлы с расширением csv.
        if path.extension().and_then(|e| e.to_str()) != Some("csv") {
            continue;
        }
        // Имя без расширения как число: "1765065600000" -> 1765065600000.
        let ts = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<u64>().ok());
        if let Some(ts) = ts {
            files.push((ts, path));
        }
    }
    files.sort_by_key(|f| f.0);
    Ok(files.into_iter().map(|f| f.1).collect())
}

/// Порог «крупного» ордера: 95-й процентиль объёма склеенных ордеров (gap 50 мс)
/// по первым n_files файлам. Фиксируется ОДИН раз для всего датасета и берётся из
/// ранних дней, чтобы не подглядывать в будущее при проверке модели на поздних днях.
fn big_qty_from_files(files: &[PathBuf], n_files: usize) -> Result<f64, Box<dyn Error>> {
    let mut q: Vec<f64> = Vec::new();
    for path in files.iter().take(n_files) {
        let data = read_trades(path.to_str().ok_or("путь не в UTF-8")?)?;
        // Объём каждого склеенного ордера (поле .0 у Order).
        q.extend(group_orders(&data, 50).iter().map(|o| o.0));
    }
    if q.is_empty() {
        return Err("нет ордеров для расчёта порога".into());
    }
    q.sort_by(|a, b| a.total_cmp(b)); // сортировка нужна для процентиля
    Ok(pct(&q, 0.95))
}

/// Строит один датасет по всем файлам папки и пишет его в out_path.
///   dir             - папка с файлами <timestamp>.csv;
///   out_path        - куда писать итоговый CSV;
///   threshold_files - сколько первых файлов использовать для порога крупного ордера.
/// Файлы читаются по одному (в памяти один день, около 1 млн строк), а не все сразу:
/// два месяца это десятки миллионов строк, на кортеж Trade приходится 56 байт.
fn build_dataset_dir(
    dir: &str,
    out_path: &str,
    threshold_files: usize,
) -> Result<(), Box<dyn Error>> {
    let files = list_trade_files(dir)?;
    if files.is_empty() {
        return Err("в папке нет csv-файлов".into());
    }

    // Один порог крупного ордера на весь датасет.
    let big_qty = big_qty_from_files(&files, threshold_files)?;
    println!(
        "файлов: {}, порог крупного ордера: {:.5} BTC",
        files.len(),
        big_qty
    );

    // Признаки всех окон (их немного: 96 окон в сутки).
    let mut feats: Vec<Feat> = Vec::new();

    // Сделки последнего окна предыдущего файла. Окно может продолжаться в следующем
    // файле, поэтому его признаки считаем только после склейки с началом нового файла.
    let mut carry: Vec<Trade> = Vec::new();

    for (k, path) in files.iter().enumerate() {
        let mut data = read_trades(path.to_str().ok_or("путь не в UTF-8")?)?;

        // Склеиваем хвост предыдущего файла и новый файл в одну последовательность.
        if !carry.is_empty() {
            let mut merged = std::mem::take(&mut carry);
            merged.append(&mut data);
            data = merged;
        }
        if data.is_empty() {
            continue;
        }

        // Последнее окно может быть не закончено: отрезаем его сделки в carry.
        // partition_point находит первую сделку последнего окна (бинарный поиск,
        // данные отсортированы по времени), split_off отдаёт хвост начиная с неё.
        let last_window = data[data.len() - 1].5 / WINDOW_MS;
        let split = data.partition_point(|r| r.5 / WINDOW_MS < last_window);
        carry = data.split_off(split);

        // Признаки всех завершённых окон файла (неполные окна пропускаются).
        feats.extend(features_15m(&data, big_qty));
        println!(
            "[{}/{}] {}  окон всего: {}",
            k + 1,
            files.len(),
            path.display(),
            feats.len()
        );
    }

    // Хвост последнего файла: считаем, только если окно оказалось полным.
    if !carry.is_empty() {
        feats.extend(features_15m(&carry, big_qty));
    }

    // Пишем строки: признаки окна t + цели из окна t+1 (только соседние окна).
    write_dataset(&feats, out_path)?;
    println!("записано окон: {}, файл: {}", feats.len(), out_path);
    Ok(())
}
