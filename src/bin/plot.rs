use plotters::prelude::*;
use std::collections::BTreeMap;
use std::path::Path;
use std::{error::Error, fs};

const OUT_FILE_NAME: &str = "./plot/metrics.png";

const METRIC_NAMES: [&str; 7] = [
    "MFE>5", "MFE>10", "MFE>15", "MFE>20", "MAE<2", "MAE<5", "MAE<10",
];
const GROUP_STRIDE: i32 = 8; // 7 метрик + 1 пропуск между бакетами

const COLORS: [RGBColor; 8] = [
    RGBColor(31, 119, 180),
    RGBColor(255, 127, 14),
    RGBColor(44, 160, 44),
    RGBColor(214, 39, 40),
    RGBColor(148, 103, 189),
    RGBColor(140, 86, 75),
    RGBColor(227, 119, 194),
    RGBColor(23, 190, 207),
];

const Y_MAX: f64 = 135.0; // запас сверху под подписи бакетов и легенду

#[derive(Debug, Clone, Default)]
struct WeightBucket {
    lo: f64,
    hi: f64,
    signals: u64,
    mfe_gt: [f64; 4], // >5, >10, >15, >20  (в %)
    mae_lt: [f64; 3], // <2, <5, <10        (в %)
    median_mfe: f64,
    median_mae: f64,
}

impl WeightBucket {
    fn label(&self) -> String {
        format!("w [{}, {})  n={}", self.lo, self.hi, self.signals)
    }
    fn values(&self) -> [f64; 7] {
        [
            self.mfe_gt[0],
            self.mfe_gt[1],
            self.mfe_gt[2],
            self.mfe_gt[3],
            self.mae_lt[0],
            self.mae_lt[1],
            self.mae_lt[2],
        ]
    }
}

/// Один прогон анализа сигналов.
#[derive(Debug, Clone, Default)]
struct SignalMetrics {
    target: f64,
    stop: f64,
    timeout: u32,
    buckets: Vec<WeightBucket>,
    date: String,
}

impl SignalMetrics {
    fn label(&self) -> String {
        format!(
            "{} t{} s{} to{}",
            self.date, self.target, self.stop, self.timeout
        )
    }
}

/// Обходит metrics/<date>/*.txt. Если only_date задан, берёт только эту дату.
fn load_all(root: &str, only_date: Option<&str>) -> Result<Vec<SignalMetrics>, Box<dyn Error>> {
    let mut dirs: Vec<_> = fs::read_dir(root)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .collect();
    dirs.sort_by_key(|e| e.file_name());

    let mut runs = Vec::new();
    for d in dirs {
        let date = d.file_name().to_string_lossy().to_string();
        if only_date.map_or(false, |f| f != date) {
            continue;
        }

        let mut files: Vec<_> = fs::read_dir(d.path())?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().map_or(false, |x| x == "txt"))
            .collect();
        files.sort();

        for p in files {
            let mut m = parse(&fs::read_to_string(&p)?);
            m.date = date.clone();
            fill_from_filename(&mut m, &p);
            if m.buckets.is_empty() {
                eprintln!("пропускаю {}: нет weight-бакетов", p.display());
                continue;
            }
            runs.push(m);
        }
    }
    Ok(runs)
}

/// report_5_5_100.txt (или report_t5_s5_to100.txt): подстраховка,
/// если Target/Stop/timeout не нашлись в самом тексте.
fn fill_from_filename(m: &mut SignalMetrics, path: &Path) {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let nums: Vec<f64> = stem
        .split('_')
        .skip(1) // "report"
        .filter_map(|s| {
            s.trim_start_matches(|c: char| !c.is_ascii_digit())
                .parse()
                .ok()
        })
        .collect();
    if nums.len() >= 3 {
        if m.target == 0.0 {
            m.target = nums[0];
        }
        if m.stop == 0.0 {
            m.stop = nums[1];
        }
        if m.timeout == 0 {
            m.timeout = nums[2] as u32;
        }
    }
}

/// Парсит текстовый вывод "SIGNAL ANALYSIS ..." из твоего примера.
fn parse(text: &str) -> SignalMetrics {
    let mut m = SignalMetrics::default();
    let mut cur: Option<WeightBucket> = None;
    let (mut mfe_i, mut mae_i) = (0, 0);

    let num = |s: &str| -> f64 { s.trim().trim_end_matches('%').trim().parse().unwrap_or(0.0) };

    for raw in text.lines() {
        let line = raw.trim();

        if let Some(rest) = line.strip_prefix("--- weight [") {
            if let Some(b) = cur.take() {
                m.buckets.push(b);
            }
            // "0.5, 0.7) : 32814 signals ---"
            let (range, tail) = rest.split_once(')').unwrap_or((rest, ""));
            let (lo, hi) = range.split_once(',').unwrap_or(("0", "0"));
            let signals = tail
                .trim_start_matches([' ', ':'])
                .split_whitespace()
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            cur = Some(WeightBucket {
                lo: num(lo),
                hi: num(hi),
                signals,
                ..Default::default()
            });
            mfe_i = 0;
            mae_i = 0;
        } else if let Some(b) = cur.as_mut() {
            if line.starts_with("MFE >") {
                if mfe_i < 4 {
                    b.mfe_gt[mfe_i] = num(line.split_once(':').map_or("0", |x| x.1));
                    mfe_i += 1;
                }
            } else if line.starts_with("MAE <") {
                if mae_i < 3 {
                    b.mae_lt[mae_i] = num(line.split_once(':').map_or("0", |x| x.1));
                    mae_i += 1;
                }
            } else if line.starts_with("median MFE") {
                // "median MFE 5.990 / median MAE 2.010"
                let t: Vec<&str> = line.split_whitespace().collect();
                if t.len() >= 7 {
                    b.median_mfe = num(t[2]);
                    b.median_mae = num(t[6]);
                }
            }
        } else if let Some(v) = line.strip_prefix("Target:") {
            m.target = num(v);
        } else if let Some(v) = line.strip_prefix("Stop:") {
            m.stop = num(v);
        } else if let Some(v) = line.strip_prefix("timeout:") {
            m.timeout = num(v) as u32;
        }
    }
    if let Some(b) = cur.take() {
        m.buckets.push(b);
    }
    m
}

fn plot(runs: &[SignalMetrics], title: &str, path: &str) -> Result<(), Box<dyn Error>> {
    if runs.is_empty() {
        return Err("нет данных для графика".into());
    }
    let n_groups = runs.iter().map(|r| r.buckets.len()).max().unwrap_or(0) as i32;
    let x_max = n_groups * GROUP_STRIDE - 2;

    if let Some(dir) = std::path::Path::new(path).parent() {
        fs::create_dir_all(dir)?;
    }

    let root = BitMapBackend::new(path, (1800, 800)).into_drawing_area();
    root.fill(&WHITE)?;

    let mut chart = ChartBuilder::on(&root)
        .caption(title, ("sans-serif", 24).into_font())
        .margin(10)
        .x_label_area_size(40)
        .y_label_area_size(50)
        .build_cartesian_2d(-1i32..x_max + 1, 0f64..Y_MAX)?;

    chart
        .configure_mesh()
        .x_labels((x_max + 2) as usize)
        .x_label_formatter(&|x| {
            let k = x.rem_euclid(GROUP_STRIDE) as usize;
            if *x >= 0 && k < METRIC_NAMES.len() {
                METRIC_NAMES[k].to_string()
            } else {
                String::new()
            }
        })
        .x_label_style(("sans-serif", 13))
        .y_desc("% signals (чем выше, тем лучше)")
        .y_label_formatter(&|y| {
            if *y <= 100.0 {
                format!("{y:.0}")
            } else {
                String::new()
            }
        })
        .light_line_style(BLACK.mix(0.06))
        .draw()?;

    // вертикальные разделители между weight-бакетами
    for g in 0..n_groups - 1 {
        let x = g * GROUP_STRIDE + GROUP_STRIDE - 1;
        chart.draw_series(LineSeries::new(vec![(x, 0.0), (x, Y_MAX)], BLACK.mix(0.35)))?;
    }

    // подписи weight-бакетов
    if let Some(r) = runs.iter().max_by_key(|r| r.buckets.len()) {
        for (g, b) in r.buckets.iter().enumerate() {
            chart.plotting_area().draw(&Text::new(
                b.label(),
                (g as i32 * GROUP_STRIDE, 128.0),
                ("sans-serif", 16).into_font().color(&BLACK),
            ))?;
        }
    }

    for (ri, run) in runs.iter().enumerate() {
        let color = COLORS[ri % COLORS.len()];
        for (g, b) in run.buckets.iter().enumerate() {
            let base = g as i32 * GROUP_STRIDE;
            let v = b.values();
            // MFE и MAE рисуем ОТДЕЛЬНЫМИ отрезками
            let mfe: Vec<(i32, f64)> = (0..4).map(|k| (base + k as i32, v[k])).collect();
            let mae: Vec<(i32, f64)> = (4..7).map(|k| (base + k as i32, v[k])).collect();

            let s = chart.draw_series(LineSeries::new(mfe, color.stroke_width(2)).point_size(4))?;
            if g == 0 {
                s.label(format!("timeout {}", run.timeout))
                    .legend(move |(x, y)| {
                        PathElement::new(vec![(x, y), (x + 20, y)], color.stroke_width(2))
                    });
            }
            chart.draw_series(LineSeries::new(mae, color.stroke_width(2)).point_size(4))?;
        }
    }

    chart
        .configure_series_labels()
        .position(SeriesLabelPosition::UpperRight)
        .label_font(("sans-serif", 16))
        .background_style(&WHITE.mix(0.9))
        .border_style(&BLACK)
        .draw()?;

    root.present()?;
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    // cargo run                -> графики для всех дат
    // cargo run -- <имя папки> -> только одна дата
    let only_date = std::env::args().nth(1);
    let runs = load_all("./tmp", only_date.as_deref())?;
    println!("загружено прогонов: {}", runs.len());

    // группировка: (дата, take, stop) -> прогоны с разными timeout
    let mut groups: BTreeMap<String, Vec<SignalMetrics>> = BTreeMap::new();
    for r in runs {
        let key = format!("{}__t{}_s{}", r.date, r.target, r.stop);
        groups.entry(key).or_default().push(r);
    }

    for (key, mut rs) in groups {
        rs.sort_by_key(|r| r.timeout);
        let title = format!(
            "take {} / stop {}   {}",
            rs[0].target, rs[0].stop, rs[0].date
        );
        let file = format!("./plot/{}.png", key.replace(':', "-"));
        plot(&rs, &title, &file)?;
        println!("{file}: {} линий", rs.len());
    }
    Ok(())
}

#[test]
fn entry_point() {
    main().unwrap()
}
