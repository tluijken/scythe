//! Scales for key snap, in REAPER's `.reascale` form: twelve characters from the root up, where
//! any character but `0` marks a key of the scale.
//!
//! The MIDI editor's key snap root and scale are the project's key signature, which REAPER
//! 7.73 saves (as an undo step, "Change key signature") in a `<KEYSIG` block of the project:
//! `0 root -1 0xMASK`, bit `n` of the mask set for the key `n` semitones above the root (D
//! natural minor: `0 2 -1 0x5AD`). A project without the block is in C major.

use crate::rpp::{Chunk, Node, tokenize};

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

    /// The scale as `KEYSIG` stores it: bit `n` for the key `n` semitones above the root.
    pub fn mask(&self) -> u16 {
        (0..12u8).filter(|&n| self.scale.has(n)).fold(0, |m, n| m | 1 << n)
    }

    /// The key snap of a `KEYSIG` root and mask: a built-in scale with those keys, else a
    /// custom one.
    pub fn from_mask(root: u8, mask: u16) -> Self {
        let mut degree = 0;
        let pattern: String = (0..12)
            .map(|n| {
                if mask & 1 << n == 0 {
                    return '0';
                }
                degree += 1;
                char::from_digit(degree, 16).map_or('1', |c| c.to_ascii_uppercase())
            })
            .collect();
        let known = builtin().into_iter().find(|s| (0..12).all(|n| s.has(n) == (mask & 1 << n != 0)));
        Self { root: root % 12, scale: known.unwrap_or_else(|| Scale::new("Custom", &pattern)) }
    }

    /// C major, which a project without a key signature is in.
    pub fn c_major() -> Self {
        Self { root: 0, scale: builtin()[0].clone() }
    }

    /// The project's key signature, from the `<KEYSIG` among its other lines.
    pub fn read_project(extra: &[Node]) -> Option<Self> {
        let chunk = extra.iter().find_map(|n| match n {
            Node::Chunk(c) if c.name() == "KEYSIG" => Some(c),
            _ => None,
        })?;
        let line = chunk.children.iter().find_map(|n| match n {
            Node::Line(l) => Some(tokenize(l)),
            Node::Chunk(_) => None,
        })?;
        let root = line.get(1)?.parse::<i64>().ok()?.rem_euclid(12) as u8;
        let mask = line.get(3)?;
        let mask = u16::from_str_radix(mask.trim_start_matches("0x").trim_start_matches("0X"), 16).ok()?;
        Some(Self::from_mask(root, mask & 0xfff))
    }

    /// Stores the key signature in the project's `<KEYSIG`, keeping a later line's key changes.
    pub fn write_project(&self, extra: &mut Vec<Node>) {
        let line = format!("0 {} -1 0x{:X}", self.root, self.mask());
        if let Some(c) = extra.iter_mut().find_map(|n| match n {
            Node::Chunk(c) if c.name() == "KEYSIG" => Some(c),
            _ => None,
        }) {
            match c.children.iter_mut().find(|n| matches!(n, Node::Line(_))) {
                Some(first) => *first = Node::Line(line),
                None => c.children.insert(0, Node::Line(line)),
            }
            return;
        }
        let mut c = Chunk::new("KEYSIG", &[]);
        c.children.push(Node::Line(line));
        extra.push(Node::Chunk(c));
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
    fn the_key_signature_is_reapers_keysig_block() {
        let saved = crate::rpp::parse("<REAPER_PROJECT\n  <KEYSIG\n    0 2 -1 0x5AD\n  >\n>\n").unwrap();
        let k = KeySnap::read_project(&saved.children).unwrap();
        assert_eq!((k.root, k.scale.name.as_str(), k.mask()), (2, "Minor", 0x5AD), "D natural minor, as REAPER saved it");
        assert_eq!(KeySnap::c_major().mask(), 0xAB5);
        let mut extra = Vec::new();
        assert!(KeySnap::read_project(&extra).is_none());
        KeySnap::from_mask(9, 0x5AD).write_project(&mut extra);
        KeySnap::from_mask(7, 0xAB5).write_project(&mut extra);
        assert_eq!(extra.len(), 1);
        let k = KeySnap::read_project(&extra).unwrap();
        assert_eq!((k.root, k.scale.name.as_str()), (7, "Major"));
        let odd = KeySnap::from_mask(0, 0b1001_0001);
        assert_eq!((odd.scale.name.as_str(), odd.scale.pattern.as_str()), ("Custom", "100020030000"));
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
