//! Take stretch markers (RPP `SM`): pins from a take position to a source position. Positions
//! are take time, the item time times the play rate; source positions are seconds into the
//! source and replace the take's start offset. Between two markers the source plays at the
//! rate that joins them, ramping from `1 - slope` to `1 + slope` times that rate when the left
//! marker has a slope. Before the first marker and after the last one it plays at rate 1.

/// One stretch marker.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StretchMarker {
    /// Take time in seconds from the item start.
    pub pos: f64,
    /// Seconds into the source.
    pub src: f64,
    /// Rate ramp to the next marker, -1 to 1 (REAPER keeps it within ±0.99 in its editor).
    pub slope: f64,
    /// The fourth field REAPER writes for markers it adds itself.
    pub flags: Option<i64>,
}

impl StretchMarker {
    pub fn new(pos: f64, src: f64) -> Self {
        Self { pos, src, slope: 0.0, flags: None }
    }
}

/// Largest slope the rate ramp takes; at 1 the rate would reach zero.
pub const MAX_SLOPE: f64 = 0.99;

/// Markers from the tokens after `SM`: `pos src [slope [flags]]` groups joined by `+`.
pub fn parse_markers(tokens: &[String]) -> Vec<StretchMarker> {
    let mut markers: Vec<StretchMarker> = tokens
        .split(|t| t == "+")
        .filter_map(|g| {
            let n = |i: usize| g.get(i).and_then(|v| v.parse::<f64>().ok());
            Some(StretchMarker {
                pos: n(0)?,
                src: n(1)?,
                slope: n(2).unwrap_or(0.0),
                flags: g.get(3).and_then(|v| v.parse().ok()),
            })
        })
        .collect();
    markers.sort_by(|a, b| a.pos.total_cmp(&b.pos));
    markers
}

/// The `SM` line for `markers`, as REAPER writes it.
pub fn write_markers(markers: &[StretchMarker], fmt: impl Fn(f64) -> String) -> Vec<String> {
    let mut out = vec!["SM".to_owned()];
    for (i, m) in markers.iter().enumerate() {
        if i > 0 {
            out.push("+".into());
        }
        out.extend([fmt(m.pos), fmt(m.src)]);
        if m.slope != 0.0 || m.flags.is_some() {
            out.push(fmt(m.slope));
        }
        if let Some(f) = m.flags {
            out.push(f.to_string());
        }
    }
    out
}

/// The segment `tau` falls in: the index of its left marker, or `None` before the first.
fn segment(markers: &[StretchMarker], tau: f64) -> Option<usize> {
    markers.partition_point(|m| m.pos <= tau).checked_sub(1)
}

/// Where in the source take time `tau` plays. `markers` must be sorted and not empty.
pub fn source_at(markers: &[StretchMarker], tau: f64) -> f64 {
    let Some(i) = segment(markers, tau) else {
        let first = &markers[0];
        return first.src + (tau - first.pos);
    };
    let a = &markers[i];
    let Some(b) = markers.get(i + 1) else { return a.src + (tau - a.pos) };
    let span = b.pos - a.pos;
    if span <= 0.0 {
        return a.src;
    }
    let u = (tau - a.pos) / span;
    let k = a.slope.clamp(-MAX_SLOPE, MAX_SLOPE);
    a.src + (b.src - a.src) * (u + k * (u * u - u))
}

/// Source seconds per take second at take time `tau`.
pub fn rate_at(markers: &[StretchMarker], tau: f64) -> f64 {
    let Some(i) = segment(markers, tau) else { return 1.0 };
    let a = &markers[i];
    let Some(b) = markers.get(i + 1) else { return 1.0 };
    let span = b.pos - a.pos;
    if span <= 0.0 {
        return 1.0;
    }
    let u = (tau - a.pos) / span;
    let k = a.slope.clamp(-MAX_SLOPE, MAX_SLOPE);
    (b.src - a.src) / span * (1.0 + k * (2.0 * u - 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn parses_and_writes_reapers_line() {
        let line = "0 -0.1 0 1 + 0.5 0.4 0.3 + 1 1.2 + 2 2.2 0 1";
        let m = parse_markers(&toks(line));
        assert_eq!(m.len(), 4);
        assert_eq!(m[0], StretchMarker { pos: 0.0, src: -0.1, slope: 0.0, flags: Some(1) });
        assert_eq!(m[1].slope, 0.3);
        let fmt = |v: f64| if v == 0.0 { "0".to_owned() } else { v.to_string() };
        assert_eq!(write_markers(&m, fmt)[1..].join(" "), line);
    }

    #[test]
    fn maps_take_time_like_reaper() {
        // Measured from REAPER renders of a ramp: `SM 0.5 0.8 + 1.5 1.3`.
        let m = parse_markers(&toks("0.5 0.8 + 1.5 1.3"));
        assert!((source_at(&m, 0.125) - 0.425).abs() < 1e-12);
        assert!((source_at(&m, 1.0) - 1.05).abs() < 1e-12);
        assert!((source_at(&m, 1.875) - 1.675).abs() < 1e-12);
        assert_eq!(rate_at(&m, 1.0), 0.5);
        assert_eq!(rate_at(&m, 1.6), 1.0);
    }

    #[test]
    fn slope_ramps_the_rate_and_keeps_the_markers() {
        let m = parse_markers(&toks("0.5 0.8 0.5 + 1.5 1.3"));
        assert!((rate_at(&m, 0.5) - 0.25).abs() < 1e-12);
        assert!((rate_at(&m, 1.5 - 1e-12) - 0.75).abs() < 1e-9);
        assert!((source_at(&m, 1.5 - 1e-12) - 1.3).abs() < 1e-9);
    }
}
