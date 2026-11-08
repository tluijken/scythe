//! A Turtle (RDF) reader for LV2 bundle descriptions: prefixes, IRIs, prefixed names, blank
//! node property lists, collections, literals and the `;` / `,` shorthands. Enough for the
//! `manifest.ttl` and plugin files that LV2 bundles ship; not a validating parser.

use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub enum Term {
    Iri(String),
    Blank(usize),
    Literal(String),
}

impl Term {
    pub fn iri(&self) -> Option<&str> {
        match self {
            Term::Iri(s) => Some(s),
            _ => None,
        }
    }

    pub fn literal(&self) -> Option<&str> {
        match self {
            Term::Literal(s) => Some(s),
            _ => None,
        }
    }

    pub fn number(&self) -> Option<f64> {
        self.literal()?.parse().ok()
    }
}

#[derive(Debug, Default)]
pub struct Graph {
    pub triples: Vec<(Term, String, Term)>,
}

impl Graph {
    /// Objects of `subject` for `predicate`.
    pub fn objects<'a>(&'a self, subject: &'a Term, predicate: &'a str) -> impl Iterator<Item = &'a Term> + 'a {
        self.triples.iter().filter(move |(s, p, _)| s == subject && p == predicate).map(|(_, _, o)| o)
    }

    pub fn object<'a>(&'a self, subject: &'a Term, predicate: &'a str) -> Option<&'a Term> {
        self.objects(subject, predicate).next()
    }

    /// Subjects that have `predicate` pointing at `object`.
    pub fn subjects<'a>(&'a self, predicate: &'a str, object: &'a Term) -> impl Iterator<Item = &'a Term> + 'a {
        self.triples.iter().filter(move |(_, p, o)| p == predicate && o == object).map(|(s, _, _)| s)
    }

    pub fn has_type(&self, subject: &Term, class: &str) -> bool {
        self.objects(subject, RDF_TYPE).any(|o| o.iri() == Some(class))
    }
}

pub const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

/// Parses `text`, resolving relative IRIs against `base` (the file's own `file://` IRI).
pub fn parse(text: &str, base: &str) -> Result<Graph, String> {
    let mut p = Parser {
        src: text.as_bytes(),
        at: 0,
        prefixes: HashMap::new(),
        base: base.to_owned(),
        graph: Graph::default(),
        blanks: 0,
        labels: HashMap::new(),
    };
    p.document()?;
    Ok(p.graph)
}

struct Parser<'a> {
    src: &'a [u8],
    at: usize,
    prefixes: HashMap<String, String>,
    base: String,
    graph: Graph,
    blanks: usize,
    labels: HashMap<String, usize>,
}

impl Parser<'_> {
    fn err<T>(&self, msg: &str) -> Result<T, String> {
        let line = self.src[..self.at.min(self.src.len())].iter().filter(|&&c| c == b'\n').count() + 1;
        Err(format!("line {line}: {msg}"))
    }

    fn skip(&mut self) {
        while self.at < self.src.len() {
            match self.src[self.at] {
                b' ' | b'\t' | b'\r' | b'\n' => self.at += 1,
                b'#' => {
                    while self.at < self.src.len() && self.src[self.at] != b'\n' {
                        self.at += 1;
                    }
                }
                _ => break,
            }
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip();
        self.src.get(self.at).copied()
    }

    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, c: u8) -> Result<(), String> {
        if self.eat(c) { Ok(()) } else { self.err(&format!("expected '{}'", c as char)) }
    }

    fn keyword(&mut self, word: &str) -> bool {
        self.skip();
        let end = self.at + word.len();
        let matches = self.src.get(self.at..end).is_some_and(|s| s.eq_ignore_ascii_case(word.as_bytes()))
            && self.src.get(end).is_none_or(|c| !c.is_ascii_alphanumeric());
        if matches {
            self.at = end;
        }
        matches
    }

    fn document(&mut self) -> Result<(), String> {
        while self.peek().is_some() {
            if self.keyword("@prefix") || self.keyword("prefix") {
                let name = self.pname_prefix()?;
                let iri = self.iriref()?;
                self.prefixes.insert(name, iri);
                self.eat(b'.');
            } else if self.keyword("@base") || self.keyword("base") {
                self.base = self.iriref()?;
                self.eat(b'.');
            } else {
                let subject = self.subject()?;
                if self.peek() != Some(b'.') {
                    self.predicate_objects(&subject)?;
                }
                self.expect(b'.')?;
            }
        }
        Ok(())
    }

    fn pname_prefix(&mut self) -> Result<String, String> {
        self.skip();
        let start = self.at;
        while self.at < self.src.len() && self.src[self.at] != b':' {
            self.at += 1;
        }
        let name = String::from_utf8_lossy(&self.src[start..self.at]).trim().to_owned();
        self.expect(b':')?;
        Ok(name)
    }

    fn iriref(&mut self) -> Result<String, String> {
        self.expect(b'<')?;
        let start = self.at;
        while self.at < self.src.len() && self.src[self.at] != b'>' {
            self.at += 1;
        }
        let raw = String::from_utf8_lossy(&self.src[start..self.at]).into_owned();
        self.at += 1;
        Ok(self.resolve(&raw))
    }

    fn resolve(&self, iri: &str) -> String {
        if iri.contains(':') {
            return iri.to_owned();
        }
        if let Some(rest) = iri.strip_prefix('#') {
            return format!("{}#{rest}", self.base.split('#').next().unwrap_or(""));
        }
        match self.base.rfind('/') {
            Some(i) => format!("{}{}", &self.base[..=i], iri),
            None => iri.to_owned(),
        }
    }

    fn name(&mut self) -> String {
        let start = self.at;
        while self.at < self.src.len() {
            let c = self.src[self.at];
            let ok = c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b':' | b'%') || c >= 0x80;
            // A '.' belongs to the name only when more name follows.
            let dot = c == b'.' && self.src.get(self.at + 1).is_some_and(|n| n.is_ascii_alphanumeric());
            if !(ok || dot) {
                break;
            }
            self.at += 1;
        }
        String::from_utf8_lossy(&self.src[start..self.at]).into_owned()
    }

    fn prefixed(&mut self) -> Result<Term, String> {
        let word = self.name();
        if let Some(label) = word.strip_prefix("_:") {
            let next = self.blanks;
            let id = *self.labels.entry(label.to_owned()).or_insert(next);
            if id == next {
                self.blanks += 1;
            }
            return Ok(Term::Blank(id));
        }
        match word.split_once(':') {
            Some((prefix, local)) => match self.prefixes.get(prefix) {
                Some(ns) => Ok(Term::Iri(format!("{ns}{local}"))),
                None => self.err(&format!("unknown prefix '{prefix}'")),
            },
            None => match word.as_str() {
                "true" | "false" => Ok(Term::Literal(word)),
                _ => self.err(&format!("unexpected '{word}'")),
            },
        }
    }

    fn subject(&mut self) -> Result<Term, String> {
        match self.peek() {
            Some(b'<') => Ok(Term::Iri(self.iriref()?)),
            Some(b'[') => self.blank_node(),
            Some(b'(') => self.collection(),
            _ => self.prefixed(),
        }
    }

    fn predicate_objects(&mut self, subject: &Term) -> Result<(), String> {
        loop {
            let predicate = if self.keyword("a") {
                RDF_TYPE.to_owned()
            } else {
                match self.peek() {
                    Some(b'<') => self.iriref()?,
                    _ => match self.prefixed()? {
                        Term::Iri(i) => i,
                        _ => return self.err("predicate must be an IRI"),
                    },
                }
            };
            loop {
                let object = self.object()?;
                self.graph.triples.push((subject.clone(), predicate.clone(), object));
                if !self.eat(b',') {
                    break;
                }
            }
            if !self.eat(b';') {
                return Ok(());
            }
            while self.eat(b';') {}
            if matches!(self.peek(), Some(b'.' | b']') | None) {
                return Ok(());
            }
        }
    }

    fn blank_node(&mut self) -> Result<Term, String> {
        self.expect(b'[')?;
        let node = Term::Blank(self.blanks);
        self.blanks += 1;
        if self.peek() != Some(b']') {
            self.predicate_objects(&node)?;
        }
        self.expect(b']')?;
        Ok(node)
    }

    fn collection(&mut self) -> Result<Term, String> {
        const FIRST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#first";
        const REST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#rest";
        const NIL: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#nil";
        self.expect(b'(')?;
        let mut items = Vec::new();
        while self.peek() != Some(b')') {
            if self.peek().is_none() {
                return self.err("unterminated collection");
            }
            items.push(self.object()?);
        }
        self.at += 1;
        let mut tail = Term::Iri(NIL.into());
        for item in items.into_iter().rev() {
            let node = Term::Blank(self.blanks);
            self.blanks += 1;
            self.graph.triples.push((node.clone(), FIRST.into(), item));
            self.graph.triples.push((node.clone(), REST.into(), tail));
            tail = node;
        }
        Ok(tail)
    }

    fn object(&mut self) -> Result<Term, String> {
        match self.peek() {
            Some(b'<') => Ok(Term::Iri(self.iriref()?)),
            Some(b'[') => self.blank_node(),
            Some(b'(') => self.collection(),
            Some(b'"' | b'\'') => self.string_literal(),
            Some(c) if c.is_ascii_digit() || matches!(c, b'+' | b'-' | b'.') => Ok(self.number()),
            _ => self.prefixed(),
        }
    }

    fn number(&mut self) -> Term {
        let start = self.at;
        while self.at < self.src.len() {
            let c = self.src[self.at];
            let dot = c == b'.' && self.src.get(self.at + 1).is_some_and(|n| n.is_ascii_digit());
            if !(c.is_ascii_digit() || matches!(c, b'+' | b'-' | b'e' | b'E') || dot) {
                break;
            }
            self.at += 1;
        }
        Term::Literal(String::from_utf8_lossy(&self.src[start..self.at]).into_owned())
    }

    fn string_literal(&mut self) -> Result<Term, String> {
        let quote = self.src[self.at];
        let long = self.src.get(self.at..self.at + 3) == Some(&[quote; 3]);
        self.at += if long { 3 } else { 1 };
        let mut out = Vec::new();
        loop {
            let Some(&c) = self.src.get(self.at) else { return self.err("unterminated string") };
            if long && self.src.get(self.at..self.at + 3) == Some(&[quote; 3]) {
                self.at += 3;
                break;
            }
            if !long && c == quote {
                self.at += 1;
                break;
            }
            if c == b'\\' {
                let e = self.src.get(self.at + 1).copied().unwrap_or(b'\\');
                out.push(match e {
                    b'n' => b'\n',
                    b't' => b'\t',
                    b'r' => b'\r',
                    other => other,
                });
                self.at += 2;
                continue;
            }
            out.push(c);
            self.at += 1;
        }
        // Language tags and datatypes do not matter to the host.
        if self.src.get(self.at) == Some(&b'@') {
            self.at += 1;
            self.name();
        } else if self.src.get(self.at..self.at + 2) == Some(b"^^") {
            self.at += 2;
            if self.peek() == Some(b'<') {
                self.iriref()?;
            } else {
                self.prefixed()?;
            }
        }
        Ok(Term::Literal(String::from_utf8_lossy(&out).into_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_an_lv2_plugin_description() {
        let ttl = r#"
@prefix lv2:  <http://lv2plug.in/ns/lv2core#> .
@prefix doap: <http://usefulinc.com/ns/doap#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .

<http://example.org/amp>
    a lv2:Plugin , lv2:AmplifierPlugin ;
    doap:name "Simple \"Amp\""@en ;
    lv2:binary <amp.so> ;
    lv2:port [
        a lv2:InputPort , lv2:ControlPort ;
        lv2:index 0 ;
        lv2:symbol "gain" ;
        lv2:default -3.5 ;
        lv2:maximum 1.0e1 ;
        lv2:scalePoint [ rdfs:label "off" ; rdf:value 0 ] ;
    ] , [
        a lv2:AudioPort, lv2:OutputPort ;
        lv2:index 1 ;
        lv2:symbol "out"
    ] .
"#
        .replace("rdf:value", "<http://www.w3.org/1999/02/22-rdf-syntax-ns#value>");
        let g = parse(&ttl, "file:///lv2/amp.lv2/amp.ttl").unwrap();
        let plugin = Term::Iri("http://example.org/amp".into());
        assert!(g.has_type(&plugin, "http://lv2plug.in/ns/lv2core#Plugin"));
        assert_eq!(g.object(&plugin, "http://usefulinc.com/ns/doap#name").unwrap().literal(), Some("Simple \"Amp\""));
        assert_eq!(
            g.object(&plugin, "http://lv2plug.in/ns/lv2core#binary").unwrap().iri(),
            Some("file:///lv2/amp.lv2/amp.so")
        );
        let ports: Vec<_> = g.objects(&plugin, "http://lv2plug.in/ns/lv2core#port").collect();
        assert_eq!(ports.len(), 2);
        assert_eq!(g.object(ports[0], "http://lv2plug.in/ns/lv2core#default").unwrap().number(), Some(-3.5));
        assert_eq!(g.object(ports[0], "http://lv2plug.in/ns/lv2core#maximum").unwrap().number(), Some(10.0));
        assert_eq!(g.object(ports[1], "http://lv2plug.in/ns/lv2core#symbol").unwrap().literal(), Some("out"));
    }
}
