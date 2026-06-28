//! History time-travel track geometry, ported 1:1 from desktop
//! `src/vault/history.ts`. All times are epoch milliseconds (f64, mirroring the
//! JS number math). Date formatting uses UTC (the desktop uses local time; only
//! the format shape is asserted, and the app can apply a local offset later).

pub const MIN: f64 = 60_000.0;
pub const HOUR: f64 = 3_600_000.0;
pub const DAY: f64 = 86_400_000.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    pub start: f64,
    pub end: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TrackEvent {
    pub id: String,
    pub ts: f64,
    pub kind: String,
    pub path: String,
}

pub fn default_view(now: f64) -> View {
    View { start: now - 7.0 * DAY, end: now + 0.4 * DAY }
}

pub fn clamp_view(mut start: f64, mut end: f64, now: f64) -> View {
    let span = end - start;
    let max_end = now + span * 0.4;
    if end > max_end {
        let sh = end - max_end;
        start -= sh;
        end -= sh;
    }
    let min_start = now - 90.0 * DAY;
    if start < min_start {
        let sh = min_start - start;
        start += sh;
        end += sh;
    }
    View { start, end }
}

pub fn to_pct(ts: f64, view: View) -> f64 {
    ((ts - view.start) / (view.end - view.start)) * 100.0
}

const STEPS: [f64; 12] = [
    5.0 * MIN,
    15.0 * MIN,
    30.0 * MIN,
    HOUR,
    3.0 * HOUR,
    6.0 * HOUR,
    12.0 * HOUR,
    DAY,
    2.0 * DAY,
    7.0 * DAY,
    14.0 * DAY,
    30.0 * DAY,
];

pub fn choose_step(span: f64) -> f64 {
    let raw = span / 6.0;
    let mut step = STEPS[STEPS.len() - 1];
    for s in STEPS {
        if s >= raw {
            step = s;
            break;
        }
    }
    step
}

pub fn clamp_span(span: f64) -> f64 {
    (MIN * 10.0).max((60.0 * DAY).min(span))
}

pub fn zoom_keeping_focus(view: View, f: f64, factor: f64, now: f64) -> View {
    let span = view.end - view.start;
    let focus = view.start + f * span;
    let ns = clamp_span(span * factor);
    clamp_view(focus - f * ns, focus - f * ns + ns, now)
}

pub fn zoom_around(view: View, center: f64, factor: f64, now: f64) -> View {
    let span = view.end - view.start;
    let f = (center - view.start) / span;
    let ns = clamp_span(span * factor);
    clamp_view(center - f * ns, center - f * ns + ns, now)
}

pub fn view_for_now(view: View, now: f64) -> View {
    if now > view.end || now < view.start {
        let span = view.end - view.start;
        View { start: now - span * 0.82, end: now + span * 0.18 }
    } else {
        view
    }
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Epoch-ms → (year, month 1-12, day, hour, minute) in UTC (Howard Hinnant's algo).
fn civil(ms: f64) -> (i64, usize, i64, i64, i64) {
    let secs = (ms / 1000.0).floor() as i64;
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let hour = rem / 3600;
    let min = (rem % 3600) / 60;
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m as usize, d, hour, min)
}

fn pad(x: i64) -> String {
    if x < 10 {
        format!("0{x}")
    } else {
        format!("{x}")
    }
}

pub fn fmt_full(ts: f64) -> String {
    let (_, m, d, h, mi) = civil(ts);
    format!("{} {}, {}:{}", MONTHS[m - 1], d, pad(h), pad(mi))
}

pub fn fmt_tick(ts: f64, step: f64) -> String {
    let (_, m, d, h, mi) = civil(ts);
    if step >= DAY {
        format!("{} {}", MONTHS[m - 1], d)
    } else {
        format!("{}:{}", pad(h), pad(mi))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AxisTick {
    pub label: String,
    pub pct: f64,
}

pub fn axis_ticks_for(view: View) -> Vec<AxisTick> {
    let step = choose_step(view.end - view.start);
    let mut out = Vec::new();
    let mut t = (view.start / step).ceil() * step;
    while t <= view.end {
        out.push(AxisTick { label: fmt_tick(t, step), pct: to_pct(t, view) });
        t += step;
    }
    out
}

pub fn color_of(kind: &str) -> &'static str {
    match kind {
        "create" => "#3fa45a",
        "edit" => "#3d63dd",
        "rename" => "#d9a93d",
        _ => "#d96a6a",
    }
}

/// Backend history (unix SECONDS) → sorted epoch-ms track events. Input tuples
/// are `(id, ts_secs, kind, path)`.
pub fn build_events(hist: &[(String, i64, String, String)]) -> Vec<TrackEvent> {
    let mut out: Vec<TrackEvent> = hist
        .iter()
        .map(|(id, ts, kind, path)| TrackEvent {
            id: id.clone(),
            ts: *ts as f64 * 1000.0,
            kind: kind.clone(),
            path: path.clone(),
        })
        .collect();
    out.sort_by(|a, b| a.ts.partial_cmp(&b.ts).unwrap());
    out
}

/// Earliest event ts (ms) per path.
pub fn create_ts_by_path(events: &[TrackEvent]) -> std::collections::HashMap<String, f64> {
    let mut m = std::collections::HashMap::new();
    for e in events {
        let entry = m.entry(e.path.clone()).or_insert(e.ts);
        if e.ts < *entry {
            *entry = e.ts;
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: f64 = 1_700_000_000_000.0;

    #[test]
    fn to_pct_maps_endpoints() {
        let v = View { start: 0.0, end: 100.0 };
        assert_eq!(to_pct(0.0, v), 0.0);
        assert_eq!(to_pct(50.0, v), 50.0);
        assert_eq!(to_pct(100.0, v), 100.0);
    }

    #[test]
    fn choose_step_picks_first_ge_span_over_6() {
        assert_eq!(choose_step(6.0 * HOUR), HOUR);
        assert_eq!(choose_step(7.0 * DAY), 2.0 * DAY);
        assert_eq!(choose_step(6.0 * 4.0 * MIN), 5.0 * MIN);
    }

    #[test]
    fn clamp_span_bounds() {
        assert_eq!(clamp_span(MIN), 10.0 * MIN);
        assert_eq!(clamp_span(100.0 * DAY), 60.0 * DAY);
        assert_eq!(clamp_span(3.0 * DAY), 3.0 * DAY);
    }

    #[test]
    fn clamp_view_keeps_end_and_start_bounds() {
        let span = 10.0 * DAY;
        let v = clamp_view(NOW + 5.0 * DAY, NOW + 5.0 * DAY + span, NOW);
        assert!((v.end - v.start - span).abs() < 1e-3);
        assert!(v.end <= NOW + span * 0.4 + 1.0);

        let v2 = clamp_view(NOW - 200.0 * DAY, NOW - 200.0 * DAY + span, NOW);
        assert!(v2.start >= NOW - 90.0 * DAY - 1.0);
        assert!((v2.end - v2.start - span).abs() < 1e-3);
    }

    #[test]
    fn zoom_keeps_focus_and_center() {
        let v = default_view(NOW);
        let f = 0.5;
        let focus_before = v.start + f * (v.end - v.start);
        let nv = zoom_keeping_focus(v, f, 0.5, NOW);
        let focus_after = nv.start + f * (nv.end - nv.start);
        assert!((focus_after - focus_before).abs() < 1.0);
        assert!(nv.end - nv.start < v.end - v.start);

        let center = NOW - 2.0 * DAY;
        let f_before = (center - v.start) / (v.end - v.start);
        let nv2 = zoom_around(v, center, 1.8, NOW);
        let f_after = (center - nv2.start) / (nv2.end - nv2.start);
        assert!((f_after - f_before).abs() < 1e-3);
    }

    #[test]
    fn view_for_now_recenters_only_when_outside() {
        let inside = View { start: NOW - DAY, end: NOW + DAY };
        assert_eq!(view_for_now(inside, NOW), inside);
        let outside = View { start: NOW - 10.0 * DAY, end: NOW - 5.0 * DAY };
        let re = view_for_now(outside, NOW);
        assert!(NOW >= re.start && NOW <= re.end);
    }

    #[test]
    fn axis_ticks_in_range() {
        let v = default_view(NOW);
        let ticks = axis_ticks_for(v);
        assert!(!ticks.is_empty());
        for t in ticks {
            assert!(t.pct >= -1.0 && t.pct <= 101.0);
        }
    }

    #[test]
    fn build_events_converts_sorts_and_earliest() {
        let evs = build_events(&[
            ("b".into(), 200, "edit".into(), "a.md".into()),
            ("a".into(), 100, "create".into(), "a.md".into()),
            ("c".into(), 150, "create".into(), "b.md".into()),
        ]);
        assert_eq!(evs.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["a", "c", "b"]);
        assert_eq!(evs[0].ts, 100_000.0);
        let created = create_ts_by_path(&evs);
        assert_eq!(created["a.md"], 100_000.0);
        assert_eq!(created["b.md"], 150_000.0);
    }

    #[test]
    fn color_of_by_kind() {
        assert_eq!(color_of("create"), "#3fa45a");
        assert_eq!(color_of("edit"), "#3d63dd");
        assert_eq!(color_of("rename"), "#d9a93d");
        assert_eq!(color_of("delete"), "#d96a6a");
    }

    #[test]
    fn time_formatting_shapes() {
        let full = fmt_full(1_636_927_400_000.0);
        // e.g. "Nov 14, 22:13" (UTC) — shape: "Mon D, HH:MM"
        let re_ok = full.len() >= 9 && full.contains(", ") && full.contains(':');
        assert!(re_ok, "got {full}");
        let tick_day = fmt_tick(1_636_927_400_000.0, DAY);
        assert!(!tick_day.contains(':'));
        let tick_hour = fmt_tick(1_636_927_400_000.0, HOUR);
        assert!(tick_hour.contains(':') && tick_hour.len() == 5);
    }
}
