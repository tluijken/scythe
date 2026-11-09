//! Reader and writer for REAPER's `.RPP` text format: a tree of `<NAME args …` / `>` chunks
//! holding whitespace-separated lines. Lines are kept verbatim so unknown content survives a
//! load/save cycle.

mod project;

pub use project::{
    ProjectIoError, autosave_path, fmt_num, fx_chain_from_text, fx_chain_to_text, project_from_rpp, project_to_rpp,
    prune_backups, read_project, resolve_media_path, save_project, track_template, tracks_from_template, write_project,
};

use std::fmt::Write as _;

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Line(String),
    Chunk(Chunk),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Chunk {
    /// Text after `<`, e.g. `TRACK {GUID}`.
    pub header: String,
    pub children: Vec<Node>,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ParseError {
    #[error("line {0}: `>` without an open chunk")]
    UnexpectedClose(usize),
    #[error("{0} chunk(s) not closed at end of file")]
    Unclosed(usize),
    #[error("file does not start with a chunk")]
    NoRoot,
    #[error("expected a {expected} chunk, found {found}")]
    WrongRoot { expected: &'static str, found: String },
}

impl Chunk {
    pub fn new(name: &str, args: &[&str]) -> Self {
        Self { header: join_tokens(std::iter::once(name).chain(args.iter().copied())), children: Vec::new() }
    }

    pub fn name(&self) -> &str {
        self.header.split_whitespace().next().unwrap_or("")
    }

    pub fn args(&self) -> Vec<String> {
        tokenize(&self.header).into_iter().skip(1).collect()
    }

    pub fn line(&mut self, tokens: impl IntoIterator<Item = impl AsRef<str>>) {
        self.children.push(Node::Line(join_tokens(tokens)));
    }

    pub fn push_chunk(&mut self, chunk: Chunk) {
        self.children.push(Node::Chunk(chunk));
    }

    pub fn chunks<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Chunk> + 'a {
        self.children.iter().filter_map(move |n| match n {
            Node::Chunk(c) if c.name() == name => Some(c),
            _ => None,
        })
    }

    /// Tokens of the first line whose first token is `key`, without the key.
    pub fn find_line(&self, key: &str) -> Option<Vec<String>> {
        self.children.iter().find_map(|n| match n {
            Node::Line(l) => {
                let toks = tokenize(l);
                (toks.first().map(String::as_str) == Some(key)).then(|| toks[1..].to_vec())
            }
            _ => None,
        })
    }
}

/// First token of a line, cheap enough to call while dispatching.
pub fn line_key(line: &str) -> &str {
    line.split_whitespace().next().unwrap_or("")
}

pub fn tokenize(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = line.trim().chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        let mut tok = String::new();
        if matches!(c, '"' | '\'' | '`') {
            chars.next();
            for ch in chars.by_ref() {
                if ch == c {
                    break;
                }
                tok.push(ch);
            }
        } else {
            while let Some(&ch) = chars.peek() {
                if ch.is_whitespace() {
                    break;
                }
                tok.push(ch);
                chars.next();
            }
        }
        out.push(tok);
    }
    out
}

/// Quotes a token the way REAPER does: bare when safe, otherwise wrapped in the first of
/// `"`, `'`, `` ` `` that does not occur in it. REAPER has no escape sequence, so when all three
/// occur the backticks are degraded to apostrophes.
pub fn quote(token: &str) -> String {
    let needs_quotes =
        token.is_empty() || token.chars().any(char::is_whitespace) || token.starts_with(['"', '\'', '`']);
    if !needs_quotes {
        return token.to_owned();
    }
    for q in ['"', '\'', '`'] {
        if !token.contains(q) {
            return format!("{q}{token}{q}");
        }
    }
    format!("`{}`", token.replace('`', "'"))
}

/// Like [`quote`] but always wraps the token, as REAPER does for file paths.
pub fn quote_always(token: &str) -> String {
    let q = quote(token);
    if q.starts_with(['"', '\'', '`']) { q } else { format!("\"{token}\"") }
}

pub fn join_tokens(tokens: impl IntoIterator<Item = impl AsRef<str>>) -> String {
    tokens.into_iter().map(|t| quote(t.as_ref())).collect::<Vec<_>>().join(" ")
}

pub fn parse(text: &str) -> Result<Chunk, ParseError> {
    let mut stack: Vec<Chunk> = Vec::new();
    let mut root: Option<Chunk> = None;
    for (no, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(header) = line.strip_prefix('<') {
            stack.push(Chunk { header: header.trim().to_owned(), children: Vec::new() });
        } else if line == ">" {
            let done = stack.pop().ok_or(ParseError::UnexpectedClose(no + 1))?;
            match stack.last_mut() {
                Some(parent) => parent.children.push(Node::Chunk(done)),
                None => {
                    root = Some(done);
                    break;
                }
            }
        } else if let Some(top) = stack.last_mut() {
            top.children.push(Node::Line(line.to_owned()));
        } else if root.is_none() {
            return Err(ParseError::NoRoot);
        }
    }
    if !stack.is_empty() {
        return Err(ParseError::Unclosed(stack.len()));
    }
    root.ok_or(ParseError::NoRoot)
}

pub fn write(chunk: &Chunk) -> String {
    let mut out = String::new();
    write_chunk(&mut out, chunk, 0);
    out
}

/// Writes nodes without an enclosing chunk, as REAPER's `.RfxChain` files hold an FX chain.
pub fn write_body(nodes: &[Node]) -> String {
    let mut out = String::new();
    write_nodes(&mut out, nodes, 0);
    out
}

fn write_chunk(out: &mut String, chunk: &Chunk, depth: usize) {
    let pad = "  ".repeat(depth);
    let _ = writeln!(out, "{pad}<{}", chunk.header);
    write_nodes(out, &chunk.children, depth + 1);
    let _ = writeln!(out, "{pad}>");
}

fn write_nodes(out: &mut String, nodes: &[Node], depth: usize) {
    let pad = "  ".repeat(depth);
    for node in nodes {
        match node {
            Node::Line(l) => {
                let _ = writeln!(out, "{pad}{l}");
            }
            Node::Chunk(c) => write_chunk(out, c, depth),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizer_handles_all_quote_styles() {
        assert_eq!(tokenize(r#"NAME "Lead vox""#), ["NAME", "Lead vox"]);
        assert_eq!(tokenize(r#"NAME 'say "hi"'"#), ["NAME", r#"say "hi""#]);
        assert_eq!(tokenize("FILE `it's \"x\".wav`"), ["FILE", "it's \"x\".wav"]);
        assert_eq!(tokenize(r#"MARKER 1 0 "" 0"#), ["MARKER", "1", "0", "", "0"]);
        assert_eq!(tokenize("  VOLPAN 1\t0  "), ["VOLPAN", "1", "0"]);
    }

    #[test]
    fn quoting_round_trips() {
        for s in ["plain", "with space", r#"has "dq" and space"#, "it's \"both\" here", "", "\"lead"] {
            let line = join_tokens(["K", s]);
            assert_eq!(tokenize(&line), ["K", s], "{line}");
        }
    }

    #[test]
    fn parse_and_write_tree() {
        let text = "<A 1\n  X 1 2\n  <B\n    Y \"a b\"\n  >\n  Z\n>\n";
        let tree = parse(text).unwrap();
        assert_eq!(tree.name(), "A");
        assert_eq!(tree.args(), ["1"]);
        assert_eq!(tree.chunks("B").count(), 1);
        assert_eq!(tree.find_line("X").unwrap(), ["1", "2"]);
        assert_eq!(write(&tree), text);
    }

    #[test]
    fn parse_errors() {
        assert_eq!(parse("<A\n"), Err(ParseError::Unclosed(1)));
        assert_eq!(parse("X\n"), Err(ParseError::NoRoot));
        assert_eq!(parse(">\n"), Err(ParseError::UnexpectedClose(1)));
    }
}
