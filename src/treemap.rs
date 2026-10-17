//! Squarified treemap layout (Bruls, Huizing, van Wijk).

#[derive(Clone, Copy, Debug)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Lay out `values` (sorted descending, all > 0) inside `area`.
/// Returns one rect per value, in the same order.
pub fn squarify(values: &[f64], area: Rect) -> Vec<Rect> {
    let mut out = vec![Rect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 }; values.len()];
    let total: f64 = values.iter().sum();
    if total <= 0.0 || area.w <= 0.0 || area.h <= 0.0 {
        return out;
    }
    let scale = (area.w as f64 * area.h as f64) / total;
    let scaled: Vec<f64> = values.iter().map(|v| v * scale).collect();

    let mut rect = area;
    let mut start = 0;
    while start < scaled.len() {
        let short = rect.w.min(rect.h) as f64;
        let mut end = start + 1;
        let mut best = worst(&scaled[start..end], short);
        while end < scaled.len() {
            let w = worst(&scaled[start..end + 1], short);
            if w > best {
                break;
            }
            best = w;
            end += 1;
        }
        let row = &scaled[start..end];
        let row_sum: f64 = row.iter().sum();
        if rect.w >= rect.h {
            // Column on the left.
            let col_w = if rect.h > 0.0 { (row_sum / rect.h as f64) as f32 } else { 0.0 };
            let mut y = rect.y;
            for (i, v) in row.iter().enumerate() {
                let h = if col_w > 0.0 { (*v / col_w as f64) as f32 } else { 0.0 };
                out[start + i] = Rect { x: rect.x, y, w: col_w, h };
                y += h;
            }
            rect.x += col_w;
            rect.w -= col_w;
        } else {
            // Row on top.
            let row_h = if rect.w > 0.0 { (row_sum / rect.w as f64) as f32 } else { 0.0 };
            let mut x = rect.x;
            for (i, v) in row.iter().enumerate() {
                let w = if row_h > 0.0 { (*v / row_h as f64) as f32 } else { 0.0 };
                out[start + i] = Rect { x, y: rect.y, w, h: row_h };
                x += w;
            }
            rect.y += row_h;
            rect.h -= row_h;
        }
        start = end;
    }
    out
}

fn worst(row: &[f64], short: f64) -> f64 {
    let sum: f64 = row.iter().sum();
    if sum <= 0.0 || short <= 0.0 {
        return f64::INFINITY;
    }
    let max = row.iter().cloned().fold(f64::MIN, f64::max);
    let min = row.iter().cloned().fold(f64::MAX, f64::min);
    let s2 = short * short;
    let sum2 = sum * sum;
    (s2 * max / sum2).max(sum2 / (s2 * min))
}
