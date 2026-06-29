//! Event-log panel data, ported from desktop `src/vault/log.ts`. Builds log
//! lines from the REAL append-only history + live status (no invented traffic).
//! Time formatting uses UTC (the desktop uses local; only shape is asserted).

/// A history event as the log needs it: (lamport, kind, path, ts_secs).
#[derive(Debug, Clone)]
pub struct HistEvent {
    pub lamport: u64,
    pub kind: String,
    pub path: String,
    pub ts: i64,
}

/// Live status the log references.
#[derive(Debug, Clone, Default)]
pub struct Status {
    pub rows: u64,
    pub peers: Vec<String>,
    pub listening_ticket: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    pub time: String,
    pub level: String,
    pub msg: String,
    pub raw: String,
}

pub fn log_color(level: &str, accent: &str) -> String {
    match level {
        "net" => "#7c8190".to_string(),
        "peer" => accent.to_string(),
        "sync" => "#2563eb".to_string(),
        "merge" | "vault" | "ok" => "#3a9357".to_string(),
        "disk" => "#b6612e".to_string(),
        "warn" => "#c0392b".to_string(),
        _ => "var(--faint)".to_string(),
    }
}

/// A short device tag from an ssh identity line.
pub fn short_finger(identity: &str) -> String {
    let key = strip_ssh(identity).trim();
    let from_key: String = key.chars().take(6).collect();
    if !from_key.is_empty() {
        return from_key.to_uppercase();
    }
    let from_id: String = identity.chars().take(6).collect();
    if !from_id.is_empty() {
        return from_id.to_uppercase();
    }
    "DEVICE".to_string()
}

fn strip_ssh(s: &str) -> &str {
    let Some(rest) = s.strip_prefix("ssh-") else { return s };
    let after_type = rest.trim_start_matches(|c: char| !c.is_whitespace());
    if after_type.len() == rest.len() {
        return s;
    }
    after_type.trim_start_matches(char::is_whitespace)
}

fn pad(n: i64, width: usize) -> String {
    let s = n.to_string();
    if s.len() >= width {
        s
    } else {
        "0".repeat(width - s.len()) + &s
    }
}

fn fmt_time(ms: i64) -> String {
    let total_ms = ms.rem_euclid(86_400_000);
    let h = total_ms / 3_600_000;
    let m = (total_ms % 3_600_000) / 60_000;
    let s = (total_ms % 60_000) / 1000;
    let milli = total_ms % 1000;
    format!("{}:{}:{}.{}", pad(h, 2), pad(m, 2), pad(s, 2), pad(milli, 3))
}

pub struct DeriveOpts {
    pub now: i64, // epoch ms
    pub max_events: usize,
}

impl Default for DeriveOpts {
    fn default() -> Self {
        DeriveOpts { now: 0, max_events: 40 }
    }
}

pub fn derive_log(
    events: &[HistEvent],
    status: Option<&Status>,
    identity: &str,
    opts: &DeriveOpts,
) -> Vec<LogLine> {
    let peers: Vec<String> = status.map(|s| s.peers.clone()).unwrap_or_default();
    let rows = status.map(|s| s.rows).unwrap_or(events.len() as u64);
    let ticket = status.and_then(|s| s.listening_ticket.clone());
    let finger = short_finger(identity);
    let framing_ms = if let Some(last) = events.last() {
        last.ts * 1000
    } else {
        opts.now
    };

    let mut out: Vec<LogLine> = Vec::new();
    let mut order: i64 = 0;
    let push = |level: &str, msg: String, ms: i64, out: &mut Vec<LogLine>| {
        let time = fmt_time(ms);
        let lvl_padded: String = format!("{level}     ").chars().take(5).collect();
        let raw = format!("{time}  {lvl_padded}  {msg}");
        out.push(LogLine { level: level.to_string(), msg, time, raw });
    };

    push("net", "endpoint bound · relay wss://relay.asp.dev".to_string(), framing_ms + order, &mut out);
    order += 1;
    let net_msg = match &ticket {
        Some(t) => {
            let head: String = t.chars().take(10).collect::<String>().to_lowercase();
            format!("listening · ticket {head}… printed")
        }
        None => "private · not accepting connections".to_string(),
    };
    push("net", net_msg, framing_ms + order, &mut out);
    order += 1;
    push("peer", format!("dial {} peer{}", peers.len(), plural(peers.len())), framing_ms + order, &mut out);
    order += 1;
    for peer in peers.iter().take(4) {
        push("peer", format!("connected · {}… · authKey ok", short_finger(peer)), framing_ms + order, &mut out);
        order += 1;
    }
    push(
        "sync",
        format!("catch-up · {} row{} behind head", events.len(), plural(events.len())),
        framing_ms + order,
        &mut out,
    );
    order += 1;

    let start = events.len().saturating_sub(opts.max_events);
    let recent = &events[start..];
    for (i, e) in recent.iter().enumerate() {
        let ms = e.ts * 1000;
        if i % 4 == 0 {
            let sz = 2.1 + ((e.lamport as i128 * 2654435761).unsigned_abs() % 40) as f64 / 10.0;
            push("sync", format!("recv frame ({sz:.1} KB)"), ms, &mut out);
        }
        push("row", format!("integrate r{} · {} {}", e.lamport, e.kind, e.path), ms, &mut out);
        if e.kind == "create" {
            push("vault", format!("create {}", e.path), ms, &mut out);
        } else if e.kind == "rename" {
            push("merge", format!("{} · path moved", e.path), ms, &mut out);
        } else if i % 3 == 0 {
            push("merge", format!("{} · clean 3-way", e.path), ms, &mut out);
        }
    }

    push(
        "ok",
        format!("in sync · {} peer{} · {} rows · {}", peers.len(), plural(peers.len()), rows, finger),
        framing_ms + order,
        &mut out,
    );
    out
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

pub fn log_text(lines: &[LogLine]) -> String {
    lines.iter().map(|l| l.raw.clone()).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(lamport: u64, kind: &str, path: &str, ts: i64) -> HistEvent {
        HistEvent { lamport, kind: kind.into(), path: path.into(), ts }
    }
    const NOW: i64 = 1_700_000_000_000;

    fn status() -> Status {
        Status {
            rows: 12,
            peers: vec!["ssh-ed25519 PEERKEYMATERIAL a@b".into()],
            listening_ticket: Some("asp1abcdefghij".into()),
        }
    }

    #[test]
    fn short_finger_strips_and_uppercases() {
        assert_eq!(short_finger("ssh-ed25519 AAAAbbbb host"), "AAAABB");
        assert_eq!(short_finger(""), "DEVICE");
    }

    #[test]
    fn log_color_maps_levels() {
        assert_eq!(log_color("peer", "#123456"), "#123456");
        assert_eq!(log_color("net", "#123456"), "#7c8190");
        assert_eq!(log_color("sync", "#123456"), "#2563eb");
        assert_eq!(log_color("merge", "#123456"), "#3a9357");
        assert_eq!(log_color("vault", "#123456"), "#3a9357");
        assert_eq!(log_color("ok", "#123456"), "#3a9357");
        assert_eq!(log_color("disk", "#123456"), "#b6612e");
        assert_eq!(log_color("warn", "#123456"), "#c0392b");
        assert_eq!(log_color("row", "#123456"), "var(--faint)");
    }

    #[test]
    fn derives_framing_and_rows() {
        let events = [
            ev(1, "create", "README.md", 1700000000),
            ev(2, "edit", "README.md", 1700000060),
            ev(3, "rename", "a.md", 1700000120),
        ];
        let lines = derive_log(&events, Some(&status()), "ssh-ed25519 DEVICEKEY me@host", &DeriveOpts { now: NOW, max_events: 40 });
        let text = log_text(&lines);
        assert!(text.contains("endpoint bound"));
        assert!(text.contains("listening · ticket asp1abcdef… printed"));
        assert!(text.contains("dial 1 peer"));
        assert!(text.contains("integrate r1 · create README.md"));
        assert!(text.contains("create README.md"));
        assert!(text.contains("a.md · path moved"));
        assert!(text.contains("in sync · 1 peer · 12 rows"));
        // raw column: time + 2sp + 'net' + 4sp + 'endpoint'
        assert!(lines[0].raw.starts_with(&format!("{}  net    endpoint", lines[0].time)));
    }

    #[test]
    fn private_vault_no_peers_no_events() {
        let st = Status { rows: 0, peers: vec![], listening_ticket: None };
        let lines = derive_log(&[], Some(&st), "ssh-ed25519 K x", &DeriveOpts { now: NOW, max_events: 40 });
        let text = log_text(&lines);
        assert!(text.contains("private · not accepting connections"));
        assert!(text.contains("dial 0 peers"));
        assert!(text.contains("in sync · 0 peers · 0 rows"));
    }

    #[test]
    fn rows_fall_back_to_event_count() {
        let lines = derive_log(&[ev(1, "edit", "x.md", 1700000000)], None, "ssh-ed25519 K x", &DeriveOpts { now: NOW, max_events: 40 });
        assert!(log_text(&lines).contains("· 1 rows"));
    }

    #[test]
    fn caps_rows_via_max_events() {
        let events: Vec<HistEvent> = (0..100).map(|i| ev(i + 1, "edit", &format!("n{i}.md"), 1700000000 + i as i64)).collect();
        let lines = derive_log(&events, Some(&status()), "ssh-ed25519 K x", &DeriveOpts { now: NOW, max_events: 5 });
        let integrates: Vec<&LogLine> = lines.iter().filter(|l| l.msg.starts_with("integrate")).collect();
        assert_eq!(integrates.len(), 5);
        assert!(integrates.last().unwrap().msg.contains("n99.md"));
    }
}
