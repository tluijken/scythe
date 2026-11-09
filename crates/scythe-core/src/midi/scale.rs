//! Scales for key snap, in REAPER's `.reascale` form: twelve characters from the root up, where
//! any character but `0` marks a key of the scale.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scale {
    pub name: String,
    /// Twelve characters, the root first.
    pub pattern: String,
}

impl Scale {
    pub fn new(name: &str, pattern: &str) -> Self {
        Self { name: name.into(), pattern: pattern.into() }
    }

    /// Whether the key `semitones` above the root belongs to the scale.
    pub fn has(&self, semitones: u8) -> bool {
        self.pattern.as_bytes().get(semitones as usize % 12).is_some_and(|&c| c != b'0')
    }
}

/// The scales Scythe offers without a `.reascale` file.
pub fn builtin() -> Vec<Scale> {
    [
        ("Major", "102034050607"),
        ("Minor", "102304056070"),
        ("Harmonic minor", "102304056007"),
        ("Melodic minor", "102304050607"),
        ("Major pentatonic", "102030050600"),
        ("Minor pentatonic", "100304050070"),
        ("Blues", "100304450070"),
        ("Dorian", "102304050670"),
        ("Phrygian", "120304056070"),
        ("Lydian", "102030450607"),
        ("Mixolydian", "102034050670"),
        ("Locrian", "120304506070"),
        ("Whole tone", "102030506070"),
        ("Chromatic", "123456789ABC"),
    ]
    .into_iter()
    .map(|(n, p)| Scale::new(n, p))
    .collect()
}

/// The scales of a `.reascale` file: lines `0 "Name" pattern`. Chords, submenus and separators
/// are skipped.
pub fn parse_reascale(text: &str) -> Vec<Scale> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            let rest = line.strip_prefix('0')?.trim_start();
            let rest = rest.strip_prefix('"')?;
            let (name, pattern) = rest.split_once('"')?;
            let pattern = pattern.trim();
            (pattern.len() == 12).then(|| Scale::new(name, pattern))
        })
        .collect()
}

/// Key snap: a root (0 is C) and a scale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeySnap {
    pub root: u8,
    pub scale: Scale,
}

impl KeySnap {
    pub fn contains(&self, key: u8) -> bool {
        self.scale.has((key as i32 - self.root as i32).rem_euclid(12) as u8)
    }

    /// The scale key nearest `key`, the lower one on a tie.
    pub fn nearest(&self, key: u8) -> u8 {
        (0..12i32)
            .flat_map(|d| [key as i32 - d, key as i32 + d])
            .find(|&k| (0..=127).contains(&k) && self.contains(k as u8))
            .unwrap_or(key as i32) as u8
    }

    /// `steps` scale keys up (or down) from `key`, which first snaps into the scale.
    pub fn step(&self, key: u8, steps: i32) -> u8 {
        let mut k = self.nearest(key) as i32;
        for _ in 0..steps.unsigned_abs() {
            let dir = steps.signum();
            let mut next = k + dir;
            while (0..=127).contains(&next) && !self.contains(next as u8) {
                next += dir;
            }
            if !(0..=127).contains(&next) {
                break;
            }
            k = next;
        }
        k as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_reaper_scale_files() {
        let text = "# comment\n-1\n0 \"Whole Tone\"        102030506070\n\n2 \"Modes\"\n0 \"Dorian\"\t          102304050670\n-2\n1 \"Tritone\"           100000500000\n";
        let s = parse_reascale(text);
        assert_eq!(s.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["Whole Tone", "Dorian"]);
    }

    #[test]
    fn snaps_and_steps_in_d_major() {
        let d = KeySnap { root: 2, scale: builtin()[0].clone() };
        assert!(d.contains(66) && !d.contains(65), "F# is in D major, F is not");
        assert_eq!(d.nearest(65), 64);
        assert_eq!(d.step(62, 1), 64);
        assert_eq!(d.step(62, 2), 66);
        assert_eq!(d.step(62, -1), 61);
    }
}
