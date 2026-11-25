//! PNG plotting helpers for the generated technical indicators.

use plotters::coord::Shift;
use plotters::prelude::*;
use plotters::style::full_palette::{ORANGE, PURPLE};
use std::error::Error;
use std::io;
use std::ops::Range;
use std::path::Path;

use crate::features::FeatureRow;

/// Draw an eight-panel dashboard for a slice of feature rows and save it as PNG.
///
/// `start..end` is a zero-based, end-exclusive range into `rows`. Plot a
/// manageable window (for example, 0..500) so the individual candles remain
/// visible when working with a large dataset.
/// `recorded_moves` contains `(index, previous, current, change_pct)` events;
/// events in the selected range are marked on the SMA-difference panel.
pub fn plot_features_png(
    path: impl AsRef<Path>,
    rows: &[FeatureRow],
    signals: &[Option<u8>],
    start: usize,
    end: usize,
    recorded_moves: &[(usize, &str, f64, f64, f64)],
) -> Result<(), Box<dyn Error>> {
    if rows.len() != signals.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "rows and signals must have the same length",
        )
        .into());
    }
    if start >= end || end > rows.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "plot range must satisfy start < end and end <= rows.len()",
        )
        .into());
    }

    let selected = &rows[start..end];
    let selected_signals = &signals[start..end];
    let x_start = selected.first().expect("validated non-empty range").index as i32;
    let x_end = selected.last().expect("validated non-empty range").index as i32 + 1;
    let x_range = x_start..x_end;

    let price_values = selected.iter().flat_map(|row| {
        [Some(row.close), row.sma1, row.sma2, row.ema1, row.ema2]
            .into_iter()
            .flatten()
    });
    let price_range = padded_range(price_values, 1.0);

    let roc_range = padded_range(selected.iter().filter_map(|row| row.roc_pct), 1.0);
    let volume_roc_range = padded_range(selected.iter().filter_map(|row| row.volume_roc_pct), 1.0);
    let momentum_range = padded_range(selected.iter().filter_map(|row| row.momentum), 1.0);
    let volume_momentum_range =
        padded_range(selected.iter().filter_map(|row| row.volume_momentum), 0.01);
    let sma_difference_range =
        padded_range(selected.iter().filter_map(|row| row.sma_difference), 1.0);

    let root = BitMapBackend::new(path.as_ref(), (1600, 2400)).into_drawing_area();
    root.fill(&WHITE)?;
    let panels = root.split_evenly((8, 1));

    // Price and moving averages with the strategy's buy/sell markers.
    {
        let mut chart = ChartBuilder::on(&panels[0])
            .caption("Close, SMA / EMA and signals", ("sans-serif", 24))
            .margin(10)
            .x_label_area_size(35)
            .y_label_area_size(90)
            .build_cartesian_2d(x_range.clone(), price_range)?;

        chart
            .configure_mesh()
            .x_desc("Candle index")
            .y_desc("Price")
            .draw()?;

        chart
            .draw_series(LineSeries::new(
                selected.iter().map(|row| (row.index as i32, row.close)),
                BLACK,
            ))?
            .label("Close")
            .legend(|(x, y)| PathElement::new(vec![(x, y), (x + 22, y)], BLACK));

        chart
            .draw_series(LineSeries::new(
                selected
                    .iter()
                    .filter_map(|row| row.sma1.map(|value| (row.index as i32, value))),
                BLUE,
            ))?
            .label("SMA1")
            .legend(|(x, y)| PathElement::new(vec![(x, y), (x + 22, y)], BLUE));

        chart
            .draw_series(LineSeries::new(
                selected
                    .iter()
                    .filter_map(|row| row.sma2.map(|value| (row.index as i32, value))),
                RED,
            ))?
            .label("SMA2")
            .legend(|(x, y)| PathElement::new(vec![(x, y), (x + 22, y)], RED));

        chart
            .draw_series(LineSeries::new(
                selected
                    .iter()
                    .filter_map(|row| row.ema1.map(|value| (row.index as i32, value))),
                CYAN,
            ))?
            .label("EMA1")
            .legend(|(x, y)| PathElement::new(vec![(x, y), (x + 22, y)], CYAN));

        chart
            .draw_series(LineSeries::new(
                selected
                    .iter()
                    .filter_map(|row| row.ema2.map(|value| (row.index as i32, value))),
                MAGENTA,
            ))?
            .label("EMA2")
            .legend(|(x, y)| PathElement::new(vec![(x, y), (x + 22, y)], MAGENTA));

        chart
            .draw_series(
                selected
                    .iter()
                    .zip(selected_signals)
                    .filter_map(|(row, signal)| {
                        (*signal == Some(1)).then_some((row.index as i32, row.close))
                    })
                    .map(|point| TriangleMarker::new(point, 6, GREEN.filled())),
            )?
            .label("Buy (1)")
            .legend(|(x, y)| TriangleMarker::new((x + 11, y), 6, GREEN.filled()));

        chart
            .draw_series(
                selected
                    .iter()
                    .zip(selected_signals)
                    .filter_map(|(row, signal)| {
                        (*signal == Some(0)).then_some((row.index as i32, row.close))
                    })
                    .map(|point| Circle::new(point, 5, RED.filled())),
            )?
            .label("Sell (0)")
            .legend(|(x, y)| Circle::new((x + 11, y), 5, RED.filled()));

        chart.configure_series_labels().border_style(BLACK).draw()?;
    }

    // RSI, with conventional 30/70 reference levels.
    {
        let mut chart = ChartBuilder::on(&panels[1])
            .caption("RSI", ("sans-serif", 20))
            .margin(10)
            .x_label_area_size(35)
            .y_label_area_size(90)
            .build_cartesian_2d(x_range.clone(), 0.0..100.0)?;
        chart
            .configure_mesh()
            .x_desc("Candle index")
            .y_desc("RSI")
            .draw()?;
        chart.draw_series(LineSeries::new(
            selected
                .iter()
                .filter_map(|row| row.rsi.map(|value| (row.index as i32, value))),
            BLUE,
        ))?;
        chart.draw_series([
            PathElement::new(vec![(x_start, 30.0), (x_end, 30.0)], RED.mix(0.5)),
            PathElement::new(vec![(x_start, 70.0), (x_end, 70.0)], RED.mix(0.5)),
        ])?;
    }

    // Stochastic oscillator.
    {
        let mut chart = ChartBuilder::on(&panels[2])
            .caption("Stochastic %K / %D", ("sans-serif", 20))
            .margin(10)
            .x_label_area_size(35)
            .y_label_area_size(90)
            .build_cartesian_2d(x_range.clone(), 0.0..100.0)?;
        chart
            .configure_mesh()
            .x_desc("Candle index")
            .y_desc("Percent")
            .draw()?;
        chart
            .draw_series(LineSeries::new(
                selected
                    .iter()
                    .filter_map(|row| row.stochastic_k.map(|value| (row.index as i32, value))),
                BLUE,
            ))?
            .label("%K")
            .legend(|(x, y)| PathElement::new(vec![(x, y), (x + 22, y)], BLUE));
        chart
            .draw_series(LineSeries::new(
                selected
                    .iter()
                    .filter_map(|row| row.stochastic_d.map(|value| (row.index as i32, value))),
                RED,
            ))?
            .label("%D")
            .legend(|(x, y)| PathElement::new(vec![(x, y), (x + 22, y)], RED));
        chart.draw_series([
            PathElement::new(vec![(x_start, 20.0), (x_end, 20.0)], BLACK.mix(0.3)),
            PathElement::new(vec![(x_start, 80.0), (x_end, 80.0)], BLACK.mix(0.3)),
        ])?;
        chart.configure_series_labels().border_style(BLACK).draw()?;
    }

    // Keep price and volume ROC on separate scales so one does not flatten the other.
    draw_single_indicator(
        &panels[3],
        selected,
        x_range.clone(),
        roc_range,
        "Price ROC",
        "Percent",
        |row| row.roc_pct,
        BLUE,
    )?;
    draw_single_indicator(
        &panels[4],
        selected,
        x_range.clone(),
        volume_roc_range,
        "Volume ROC",
        "Percent",
        |row| row.volume_roc_pct,
        GREEN,
    )?;

    // Price momentum.
    draw_single_indicator(
        &panels[5],
        selected,
        x_range.clone(),
        momentum_range,
        "Price momentum",
        "Price change",
        |row| row.momentum,
        BLUE,
    )?;

    // Volume momentum.
    draw_single_indicator(
        &panels[6],
        selected,
        x_range.clone(),
        volume_momentum_range,
        "Volume momentum",
        "Volume change",
        |row| row.volume_momentum,
        GREEN,
    )?;

    // Absolute SMA spread. The zero line marks a crossover/equality.
    {
        let mut chart = ChartBuilder::on(&panels[7])
            .caption("SMA1 - SMA2 difference", ("sans-serif", 20))
            .margin(10)
            .x_label_area_size(35)
            .y_label_area_size(90)
            .build_cartesian_2d(x_range, sma_difference_range)?;
        chart
            .configure_mesh()
            .x_desc("Candle index")
            .y_desc("Price units")
            .draw()?;
        chart.draw_series(LineSeries::new(
            selected
                .iter()
                .filter_map(|row| row.sma_difference.map(|value| (row.index as i32, value))),
            PURPLE,
        ))?;

        let first_visible_index = selected.first().expect("validated non-empty range").index;
        let last_visible_index = selected.last().expect("validated non-empty range").index;
        chart
            .draw_series(recorded_moves.iter().filter_map(
                |&(index, direction, _previous, current, _change_pct)| {
                    if index < first_visible_index || index > last_visible_index {
                        return None;
                    }

                    let color = match direction {
                        "down->up" => GREEN,
                        "up->down" => RED,
                        _ => return None,
                    };

                    Some(Circle::new((index as i32, current), 4, color.filled()))
                },
            ))?
            .label("SMA difference reversal")
            .legend(|(x, y)| Circle::new((x + 11, y), 7, ORANGE.filled()));

        chart.draw_series(std::iter::once(PathElement::new(
            vec![(x_start, 0.0), (x_end, 0.0)],
            BLACK.mix(0.5),
        )))?;
        chart.configure_series_labels().border_style(BLACK).draw()?;
    }

    root.present()?;
    Ok(())
}

fn draw_single_indicator<DB, F>(
    area: &DrawingArea<DB, Shift>,
    rows: &[FeatureRow],
    x_range: Range<i32>,
    y_range: Range<f64>,
    title: &str,
    y_desc: &str,
    value: F,
    color: RGBColor,
) -> Result<(), Box<dyn Error>>
where
    DB: DrawingBackend,
    DB::ErrorType: Error + Send + Sync + 'static,
    F: Fn(&FeatureRow) -> Option<f64>,
{
    let mut chart = ChartBuilder::on(area)
        .caption(title, ("sans-serif", 20))
        .margin(10)
        .x_label_area_size(35)
        .y_label_area_size(90)
        .build_cartesian_2d(x_range, y_range)?;
    chart
        .configure_mesh()
        .x_desc("Candle index")
        .y_desc(y_desc)
        .draw()?;
    chart.draw_series(LineSeries::new(
        rows.iter()
            .filter_map(|row| value(row).map(|value| (row.index as i32, value))),
        color,
    ))?;
    Ok(())
}

fn padded_range(values: impl Iterator<Item = f64>, minimum_padding: f64) -> Range<f64> {
    let values: Vec<f64> = values.filter(|value| value.is_finite()).collect();
    if values.is_empty() {
        return -1.0..1.0;
    }
    let min = values.iter().copied().fold(f64::INFINITY, f64::min);
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let padding = ((max - min).abs() * 0.05).max(minimum_padding);
    (min - padding)..(max + padding)
}
