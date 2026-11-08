//! JSFX file reading (`file_open` and friends) with REAPER's rules, measured with probe
//! effects: a RIFF/WAVE file reads as its samples; anything else as text, one value per item.

/// An open file and where reading has got to.
pub(super) enum OpenFile {
    Samples { data: Vec<f64>, pos: usize, channels: usize, rate: u32 },
    Text { bytes: Vec<u8>, pos: usize, skip: bool },
}

impl OpenFile {
    pub(super) fn new(bytes: Vec<u8>) -> Self {
        match parse_wav(&bytes) {
            Some((data, channels, rate)) => OpenFile::Samples { data, pos: 0, channels, rate },
            None => OpenFile::Text { bytes, pos: 0, skip: false },
        }
    }

    pub(super) fn rewind(&mut self) {
        match self {
            OpenFile::Samples { pos, .. } => *pos = 0,
            OpenFile::Text { pos, skip, .. } => (*pos, *skip) = (0, false),
        }
    }

    /// Samples left, or for text whether any bytes are left.
    pub(super) fn avail(&self) -> f64 {
        match self {
            OpenFile::Samples { data, pos, .. } => (data.len() - pos) as f64,
            OpenFile::Text { bytes, pos, .. } => (*pos < bytes.len()) as u8 as f64,
        }
    }

    pub(super) fn next(&mut self) -> Option<f64> {
        match self {
            OpenFile::Samples { data, pos, .. } => {
                let v = data.get(*pos).copied()?;
                *pos += 1;
                Some(v)
            }
            OpenFile::Text { bytes, pos, skip } => next_text_value(bytes, pos, skip),
        }
    }

    /// The rest of the current line, for `file_string`.
    pub(super) fn line(&mut self) -> Option<String> {
        let OpenFile::Text { bytes, pos, skip } = self else { return None };
        if *pos >= bytes.len() {
            return None;
        }
        let end = bytes[*pos..].iter().position(|&b| b == b'\n').map_or(bytes.len(), |i| *pos + i + 1);
        let line = String::from_utf8_lossy(&bytes[*pos..end]).into_owned();
        (*pos, *skip) = (end, false);
        Some(line)
    }

    pub(super) fn riff(&self) -> Option<(usize, u32)> {
        match self {
            OpenFile::Samples { channels, rate, .. } => Some((*channels, *rate)),
            OpenFile::Text { .. } => None,
        }
    }
}

fn is_separator(b: u8) -> bool {
    b == b',' || b == b'\n'
}

/// Items run to the next comma or newline. Each read evaluates the next item as far as it
/// forms an expression (`7.5e1 -8` is 67, `12abc` is 12, a name is 0) and leaves the rest of
/// it to be skipped by the next read; items starting with `/` are comments.
fn next_text_value(bytes: &[u8], pos: &mut usize, skip: &mut bool) -> Option<f64> {
    if std::mem::take(skip) {
        while *pos < bytes.len() && !is_separator(bytes[*pos]) {
            *pos += 1;
        }
        *pos = (*pos + 1).min(bytes.len());
    }
    while *pos < bytes.len() {
        let end = bytes[*pos..].iter().position(|&b| is_separator(b)).map_or(bytes.len(), |i| *pos + i);
        let mut start = *pos;
        while start < end && matches!(bytes[start], b' ' | b'\t' | b'\r') {
            start += 1;
        }
        if start < end && bytes[start] != b'/' {
            let mut p = Expr { s: &bytes[start..end], at: 0 };
            if let Some(v) = p.sum() {
                *pos = start + p.at;
                *skip = true;
                return Some(v);
            }
        }
        *pos = (end + 1).min(bytes.len());
    }
    None
}

/// The longest leading arithmetic expression of a text item.
struct Expr<'a> {
    s: &'a [u8],
    at: usize,
}

impl Expr<'_> {
    fn blank(&mut self) {
        while self.at < self.s.len() && matches!(self.s[self.at], b' ' | b'\t' | b'\r') {
            self.at += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.blank();
        self.s.get(self.at).copied()
    }

    fn sum(&mut self) -> Option<f64> {
        let mut v = self.product()?;
        while let Some(op @ (b'+' | b'-')) = self.peek() {
            let back = self.at;
            self.at += 1;
            match self.product() {
                Some(r) => v = if op == b'+' { v + r } else { v - r },
                None => {
                    self.at = back;
                    break;
                }
            }
        }
        Some(v)
    }

    fn product(&mut self) -> Option<f64> {
        let mut v = self.unary()?;
        while let Some(op @ (b'*' | b'/')) = self.peek() {
            let back = self.at;
            self.at += 1;
            match self.unary() {
                Some(r) => v = if op == b'*' { v * r } else { v / r },
                None => {
                    self.at = back;
                    break;
                }
            }
        }
        Some(v)
    }

    fn unary(&mut self) -> Option<f64> {
        match self.peek()? {
            b'-' => {
                self.at += 1;
                self.unary().map(|v| -v)
            }
            b'+' => {
                self.at += 1;
                self.unary()
            }
            b'(' => {
                let back = self.at;
                self.at += 1;
                let v = self.sum();
                if v.is_some() && self.peek() == Some(b')') {
                    self.at += 1;
                    v
                } else {
                    self.at = back;
                    None
                }
            }
            c if c.is_ascii_alphabetic() || c == b'_' => {
                while self.at < self.s.len()
                    && (self.s[self.at].is_ascii_alphanumeric() || b"_.".contains(&self.s[self.at]))
                {
                    self.at += 1;
                }
                Some(0.0)
            }
            _ => self.number(),
        }
    }

    fn number(&mut self) -> Option<f64> {
        let s = self.s;
        let start = self.at;
        if s[start..].starts_with(b"0x") || s[start..].starts_with(b"0X") {
            let digits = s[start + 2..].iter().take_while(|b| b.is_ascii_hexdigit()).count();
            if digits > 0 {
                self.at = start + 2 + digits;
                return u64::from_str_radix(std::str::from_utf8(&s[start + 2..self.at]).ok()?, 16)
                    .ok()
                    .map(|v| v as f64);
            }
        }
        let mut end = start + s[start..].iter().take_while(|b| b.is_ascii_digit() || **b == b'.').count();
        if end == start || !s[start..end].iter().any(u8::is_ascii_digit) {
            return None;
        }
        if matches!(s.get(end), Some(b'e' | b'E')) {
            let sign = usize::from(matches!(s.get(end + 1), Some(b'+' | b'-')));
            let digits = s[(end + 1 + sign).min(s.len())..].iter().take_while(|b| b.is_ascii_digit()).count();
            if digits > 0 {
                end += 1 + sign + digits;
            }
        }
        self.at = end;
        std::str::from_utf8(&s[start..end]).ok()?.parse().ok()
    }
}

/// Interleaved samples, channels and rate of a RIFF/WAVE file in integer PCM (8 to 32 bits)
/// or floating point.
fn parse_wav(b: &[u8]) -> Option<(Vec<f64>, usize, u32)> {
    if b.len() < 12 || &b[..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return None;
    }
    let u16_at = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
    let u32_at = |i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
    let mut at = 12;
    let mut format = None;
    while at + 8 <= b.len() {
        let size = u32_at(at + 4) as usize;
        let body = at + 8;
        let end = (body + size).min(b.len());
        match &b[at..at + 4] {
            b"fmt " if size >= 16 && end >= body + 16 => {
                let mut tag = u16_at(body);
                if tag == 0xfffe && size >= 26 && end >= body + 26 {
                    tag = u16_at(body + 24);
                }
                format = Some((tag, u16_at(body + 2) as usize, u32_at(body + 4), u16_at(body + 14) as usize));
            }
            b"data" => {
                let (tag, channels, rate, bits) = format?;
                let data = &b[body..end];
                let samples: Vec<f64> = match (tag, bits) {
                    (1, 8) => data.iter().map(|&v| (v as f64 - 128.0) / 128.0).collect(),
                    (1, 16) => {
                        data.as_chunks::<2>().0.iter().map(|&c| i16::from_le_bytes(c) as f64 / 32768.0).collect()
                    }
                    (1, 24) => data
                        .as_chunks::<3>()
                        .0
                        .iter()
                        .map(|&[a, b, c]| (i32::from_le_bytes([0, a, b, c]) >> 8) as f64 / 8_388_608.0)
                        .collect(),
                    (1, 32) => data
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|&c| i32::from_le_bytes(c) as f64 / 2_147_483_648.0)
                        .collect(),
                    (3, 32) => data.as_chunks::<4>().0.iter().map(|&c| f32::from_le_bytes(c) as f64).collect(),
                    (3, 64) => data.as_chunks::<8>().0.iter().map(|&c| f64::from_le_bytes(c)).collect(),
                    _ => return None,
                };
                return Some((samples, channels.max(1), rate));
            }
            _ => {}
        }
        at = body + size + (size & 1);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(text: &str) -> Vec<f64> {
        let mut f = OpenFile::new(text.as_bytes().to_vec());
        std::iter::from_fn(|| f.next()).collect()
    }

    #[test]
    fn text_items_read_like_reaper() {
        // What REAPER's file_var read from this text.
        let t = "1 2, 3\n4\t(5) x\n// 6\n7.5e1 -8\n0x10 \n  9; 10\nabc 11\n12abc\n-.5 +3\n13 // 14\n/* 15 */ 16\n";
        assert_eq!(values(t), [1.0, 3.0, 4.0, 67.0, 16.0, 9.0, 0.0, 12.0, 2.5, 13.0]);
        let mut f = OpenFile::new(b"0\t\t(0) C\n1\n12\t\t(12) C\n".to_vec());
        for v in [0.0, 1.0, 12.0] {
            assert_eq!(f.next(), Some(v));
            assert_eq!(f.avail(), 1.0, "bytes are left after each value");
        }
        assert_eq!((f.next(), f.avail()), (None, 0.0));
    }

    #[test]
    fn wav_reads_as_scaled_samples() {
        let mut b = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
        b.extend(16u32.to_le_bytes());
        b.extend([1, 0, 2, 0]);
        b.extend(44100u32.to_le_bytes());
        b.extend((44100u32 * 4).to_le_bytes());
        b.extend([4, 0, 16, 0]);
        b.extend(b"data");
        b.extend(4u32.to_le_bytes());
        b.extend(16384i16.to_le_bytes());
        b.extend((-32768i16).to_le_bytes());
        let mut f = OpenFile::new(b);
        assert_eq!(f.riff(), Some((2, 44100)));
        assert_eq!((f.avail(), f.next(), f.next(), f.next()), (2.0, Some(0.5), Some(-1.0), None));
    }
}
