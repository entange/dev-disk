use std::time::{Duration, SystemTime};

/// Decimal units, matching Finder.
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if n < 1000 {
        return format!("{n} B");
    }
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1000.0 && u < UNITS.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if v >= 100.0 {
        format!("{v:.0} {}", UNITS[u])
    } else if v >= 10.0 {
        format!("{v:.1} {}", UNITS[u])
    } else {
        format!("{v:.2} {}", UNITS[u])
    }
}

pub fn count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

pub fn duration(d: Duration) -> String {
    let s = d.as_secs_f64();
    if s < 1.0 {
        format!("{:.0} ms", s * 1000.0)
    } else if s < 60.0 {
        format!("{s:.1} s")
    } else {
        format!("{}m {:02}s", s as u64 / 60, s as u64 % 60)
    }
}

pub fn age(t: Option<SystemTime>) -> String {
    let Some(t) = t else { return "—".into() };
    let Ok(d) = SystemTime::now().duration_since(t) else { return "just now".into() };
    let days = d.as_secs() / 86_400;
    match days {
        0 => "today".into(),
        1 => "yesterday".into(),
        2..=30 => format!("{days} days ago"),
        31..=364 => format!("{} mo ago", days / 30),
        _ => format!("{:.1} yr ago", days as f64 / 365.0),
    }
}

pub fn age_days(t: Option<SystemTime>) -> u64 {
    t.and_then(|t| SystemTime::now().duration_since(t).ok()).map(|d| d.as_secs() / 86_400).unwrap_or(0)
}
