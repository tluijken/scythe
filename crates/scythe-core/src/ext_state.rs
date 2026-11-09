//! Project extended state (`SetProjExtState`): string values by section and key, stored in the
//! project's `<EXTSTATE` chunk. REAPER upper-cases sections and keys, keeps them sorted, and
//! writes a value that does not fit on one line as a `<BIN KEY` chunk of base64 lines.
//! Sections Scythe does not touch keep their exact text.

use base64::Engine as _;

use crate::model::Project;
use crate::rpp::{Chunk, Node, quote, tokenize};

const CHUNK: &str = "EXTSTATE";
const BIN_LINE: usize = 96;
/// Longer values go into a `<BIN` chunk so no line gets unwieldy.
const MAX_LINE_VALUE: usize = 256;

impl Project {
    pub fn ext_state(&self, section: &str, key: &str) -> Option<String> {
        let (section, key) = (section.to_uppercase(), key.to_uppercase());
        let sec = self.ext_state_chunk()?.chunks(&section).next()?;
        sec.children.iter().find_map(|n| entry(n).filter(|(k, _)| *k == key).map(|(_, v)| v))
    }

    /// Every key and value of `section`, in stored order.
    pub fn ext_state_section(&self, section: &str) -> Vec<(String, String)> {
        let section = section.to_uppercase();
        let Some(sec) = self.ext_state_chunk().and_then(|c| c.chunks(&section).next()) else { return Vec::new() };
        sec.children.iter().filter_map(entry).collect()
    }

    /// Sets a value, or removes it with `None` or an empty value as REAPER does.
    pub fn set_ext_state(&mut self, section: &str, key: &str, value: Option<&str>) {
        let (section, key) = (section.to_uppercase(), key.to_uppercase());
        let value = value.filter(|v| !v.is_empty());
        if value.is_none() && self.ext_state(&section, &key).is_none() {
            return;
        }
        let at = match self.extra.iter().position(|n| matches!(n, Node::Chunk(c) if c.name() == CHUNK)) {
            Some(i) => i,
            None => {
                self.extra.push(Node::Chunk(Chunk::new(CHUNK, &[])));
                self.extra.len() - 1
            }
        };
        let Node::Chunk(ext) = &mut self.extra[at] else { unreachable!() };
        let sec_at = match ext.children.iter().position(|n| matches!(n, Node::Chunk(c) if c.name() == section)) {
            Some(i) => i,
            None => {
                let i = ext
                    .children
                    .iter()
                    .position(|n| matches!(n, Node::Chunk(c) if c.name() > section.as_str()))
                    .unwrap_or(ext.children.len());
                ext.children.insert(i, Node::Chunk(Chunk { header: section.clone(), children: Vec::new() }));
                i
            }
        };
        let Node::Chunk(sec) = &mut ext.children[sec_at] else { unreachable!() };
        sec.children.retain(|n| entry(n).is_none_or(|(k, _)| k != key));
        if let Some(v) = value {
            let i =
                sec.children.iter().position(|n| entry(n).is_some_and(|(k, _)| k > key)).unwrap_or(sec.children.len());
            sec.children.insert(i, entry_node(&key, v));
        } else if sec.children.is_empty() {
            ext.children.remove(sec_at);
            if ext.children.is_empty() {
                self.extra.remove(at);
            }
        }
    }

    fn ext_state_chunk(&self) -> Option<&Chunk> {
        self.extra.iter().find_map(|n| match n {
            Node::Chunk(c) if c.name() == CHUNK => Some(c),
            _ => None,
        })
    }
}

/// A `KEY value` line or a `<BIN KEY` chunk.
fn entry(node: &Node) -> Option<(String, String)> {
    match node {
        Node::Line(l) => {
            let mut toks = tokenize(l).into_iter();
            let key = toks.next()?;
            Some((key, toks.next().unwrap_or_default()))
        }
        Node::Chunk(c) if c.name() == "BIN" => {
            let key = c.args().into_iter().next()?;
            let text: String = c
                .children
                .iter()
                .filter_map(|n| match n {
                    Node::Line(l) => Some(l.trim()),
                    _ => None,
                })
                .collect();
            let bytes = base64::engine::general_purpose::STANDARD.decode(text).ok()?;
            Some((key, String::from_utf8_lossy(&bytes).into_owned()))
        }
        _ => None,
    }
}

/// One token can hold the value unless it has a line break or every quote character.
fn fits_line(v: &str) -> bool {
    v.len() <= MAX_LINE_VALUE && !v.contains(['\n', '\r']) && !['"', '\'', '`'].iter().all(|q| v.contains(*q))
}

fn entry_node(key: &str, value: &str) -> Node {
    if fits_line(value) {
        return Node::Line(format!("{} {}", quote(key), quote(value)));
    }
    let b64 = base64::engine::general_purpose::STANDARD;
    let mut c = Chunk::new("BIN", &[key]);
    for part in value.as_bytes().chunks(BIN_LINE) {
        c.children.push(Node::Line(b64.encode(part)));
    }
    Node::Chunk(c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpp::{project_from_rpp, project_to_rpp};

    /// What REAPER 7.73 saved after `SetProjExtState` calls.
    const SAVED: &str = r#"<REAPER_PROJECT 0.1 "7.73/linux-x86_64" 0
  <EXTSTATE
    <OTHER
      K <chunk-like
    >
    <SCYTHE_SETLISTS
      "HAS SPACE" v
      LIST1 "Main set|{AAA}|Song A"
      <BIN LIST2
        aGFzICJxdW90ZXMiIGFuZCAnc2luZ2xlJyBhbmQgYGJhY2tgIGFuZCBzcGFjZXMgIGRvdWJsZQ==
      >
      <BIN MULTI
        bGluZTEKbGluZTI=
      >
    >
  >
>
"#;

    #[test]
    fn reads_reaper_lines_and_bin_chunks() {
        let p = project_from_rpp(SAVED).unwrap();
        assert_eq!(p.ext_state("scythe_setlists", "list1").as_deref(), Some("Main set|{AAA}|Song A"));
        assert_eq!(
            p.ext_state("SCYTHE_SETLISTS", "LIST2").as_deref(),
            Some(r#"has "quotes" and 'single' and `back` and spaces  double"#)
        );
        assert_eq!(p.ext_state("SCYTHE_SETLISTS", "multi").as_deref(), Some("line1\nline2"));
        assert_eq!(p.ext_state("SCYTHE_SETLISTS", "has space").as_deref(), Some("v"));
        assert_eq!(p.ext_state("other", "k").as_deref(), Some("<chunk-like"));
        assert_eq!(p.ext_state("other", "missing"), None);
    }

    #[test]
    fn writes_sorted_and_keeps_other_sections() {
        let mut p = project_from_rpp(SAVED).unwrap();
        p.set_ext_state("Scythe_Setlists", "a key", Some("x"));
        p.set_ext_state("Scythe_Setlists", "MULTI", Some("one\ntwo"));
        p.set_ext_state("Scythe_Setlists", "LIST2", None);
        p.set_ext_state("Alpha", "k", Some("v"));
        let text = project_to_rpp(&p);
        let q = project_from_rpp(&text).unwrap();
        assert_eq!(q.ext_state("scythe_setlists", "multi").as_deref(), Some("one\ntwo"));
        assert_eq!(q.ext_state("scythe_setlists", "list2"), None);
        let keys: Vec<String> = q.ext_state_section("scythe_setlists").into_iter().map(|(k, _)| k).collect();
        assert_eq!(keys, ["A KEY", "HAS SPACE", "LIST1", "MULTI"]);
        assert!(text.contains("    <ALPHA\n      K v\n    >\n    <OTHER\n      K <chunk-like\n    >"), "{text}");
    }

    #[test]
    fn removing_the_last_value_drops_the_chunk() {
        let mut p = Project::default();
        p.set_ext_state("s", "k", Some(&"long ".repeat(100)));
        assert_eq!(p.ext_state("S", "K"), Some("long ".repeat(100)));
        assert!(project_to_rpp(&p).contains("<BIN K"));
        p.set_ext_state("s", "k", Some(""));
        assert!(p.extra.is_empty());
    }
}
