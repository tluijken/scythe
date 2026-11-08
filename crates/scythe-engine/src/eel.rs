//! EEL2, the expression language of JSFX: a compiler from source to a tree of resolved
//! operations and an interpreter for it. Every value is an f64; variables are global (names
//! are case-insensitive); `x[i]` addresses the shared memory. User functions are inlined at
//! each call with their parameters, locals and instance variables renamed, as EEL2 itself does.

use std::collections::HashMap;

/// Values closer than this count as equal, and closer than this to zero as false.
const CLOSE: f64 = 0.00001;
/// Memory slots, as JSFX gives each effect.
pub const MEM_SIZE: usize = 8 * 1024 * 1024;
const BLOCK: usize = 65536;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Op {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    Or,
    And,
    Xor,
    Shl,
    Shr,
    Eq,
    ExactEq,
    Ne,
    ExactNe,
    Lt,
    Gt,
    Le,
    Ge,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Builtin {
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Atan2,
    Sqr,
    Sqrt,
    Pow,
    Exp,
    Log,
    Log10,
    Abs,
    Min,
    Max,
    Sign,
    Rand,
    Floor,
    Ceil,
    Invsqrt,
    Memset,
    Memcpy,
    Freembuf,
    Spl,
    Slider,
    MidiRecv,
    MidiSend,
    Printf,
    Sprintf,
    Strcpy,
    Strcat,
    Strlen,
    Strcmp,
    Stricmp,
    StrGetchar,
    MemTop,
    TimePrecise,
    /// No more parameter changes queued in this block (`slider_next_chg`).
    NoChange,
    /// Accepted and ignored (UI and host notifications).
    Nop,
}

impl Builtin {
    fn from_name(name: &str) -> Option<(Self, usize)> {
        use Builtin::*;
        Some(match name {
            "sin" => (Sin, 1),
            "cos" => (Cos, 1),
            "tan" => (Tan, 1),
            "asin" => (Asin, 1),
            "acos" => (Acos, 1),
            "atan" => (Atan, 1),
            "atan2" => (Atan2, 2),
            "sqr" => (Sqr, 1),
            "sqrt" => (Sqrt, 1),
            "pow" => (Pow, 2),
            "exp" => (Exp, 1),
            "log" => (Log, 1),
            "log10" => (Log10, 1),
            "abs" => (Abs, 1),
            "min" => (Min, 2),
            "max" => (Max, 2),
            "sign" => (Sign, 1),
            "rand" => (Rand, 1),
            "floor" => (Floor, 1),
            "ceil" => (Ceil, 1),
            "invsqrt" => (Invsqrt, 1),
            "memset" => (Memset, 3),
            "memcpy" => (Memcpy, 3),
            "freembuf" => (Freembuf, 1),
            "spl" => (Spl, 1),
            "slider" => (Slider, 1),
            // Four arguments, or three with both data bytes in one (`msg23`).
            "midirecv" => (MidiRecv, usize::MAX),
            "midisend" => (MidiSend, usize::MAX),
            "printf" => (Printf, usize::MAX),
            "sprintf" => (Sprintf, usize::MAX),
            "strcpy" => (Strcpy, 2),
            "strcat" => (Strcat, 2),
            "strlen" => (Strlen, 1),
            "strcmp" => (Strcmp, 2),
            "stricmp" => (Stricmp, 2),
            "str_getchar" => (StrGetchar, usize::MAX),
            "__memtop" => (MemTop, 0),
            "time_precise" => (TimePrecise, usize::MAX),
            "slider_next_chg" => (NoChange, 2),
            "slider_automate" | "sliderchange" | "slider_show" | "export_buffer_to_project" | "get_pinmapper_flags" => {
                (Nop, usize::MAX)
            }
            _ => return None,
        })
    }
}

#[derive(Debug, Clone)]
enum Node {
    Num(f64),
    Var(usize),
    /// `base[index]`; `gmem[index]` uses the shared memory, which here is the same.
    Mem(Box<Node>, Box<Node>),
    Assign(Box<Node>, Box<Node>),
    OpAssign(Op, Box<Node>, Box<Node>),
    Bin(Op, Box<Node>, Box<Node>),
    Neg(Box<Node>),
    Not(Box<Node>),
    AndAlso(Box<Node>, Box<Node>),
    OrElse(Box<Node>, Box<Node>),
    Cond(Box<Node>, Box<Node>, Option<Box<Node>>),
    Seq(Vec<Node>),
    While(Box<Node>),
    Loop(Box<Node>, Box<Node>),
    Call(Builtin, Vec<Node>),
}

// ---- lexer ---------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Ident(String),
    Str(String),
    Sym(&'static str),
}

const SYMBOLS: [&str; 36] = [
    "===", "!==", "<<=", ">>=", "==", "!=", "<=", ">=", "&&", "||", "<<", ">>", "+=", "-=", "*=", "/=", "%=", "^=",
    "|=", "&=", "~=", "+", "-", "*", "/", "%", "^", "|", "&", "~", "<", ">", "=", "!", "?", ":",
];

fn lex(src: &str) -> Result<Vec<Tok>, String> {
    let b = src.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if src[i..].starts_with("//") {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if src[i..].starts_with("/*") {
            i = src[i + 2..].find("*/").map_or(b.len(), |e| i + 2 + e + 2);
        } else if c.is_ascii_digit() || (c == b'.' && b.get(i + 1).is_some_and(u8::is_ascii_digit)) {
            if src[i..].starts_with("0x") || src[i..].starts_with("0X") {
                let s = i + 2;
                i = s;
                while i < b.len() && b[i].is_ascii_hexdigit() {
                    i += 1;
                }
                out.push(Tok::Num(i64::from_str_radix(&src[s..i], 16).map_err(|e| e.to_string())? as f64));
            } else {
                let s = i;
                while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
                    i += 1;
                }
                if i < b.len()
                    && (b[i] == b'e' || b[i] == b'E')
                    && b.get(i + 1).is_some_and(|d| d.is_ascii_digit() || *d == b'-' || *d == b'+')
                {
                    i += 2;
                    while i < b.len() && b[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                out.push(Tok::Num(src[s..i].parse().map_err(|_| format!("bad number {}", &src[s..i]))?));
            }
        } else if c == b'$' {
            // `$pi`, `$e`, `$phi`, `$'c'` and `$xFF`.
            let rest = &src[i + 1..];
            let lower = rest[..rest.len().min(3)].to_ascii_lowercase();
            if let Some(quoted) = rest.strip_prefix('\'') {
                let end = quoted.find('\'').ok_or("unterminated character")?;
                let v = quoted[..end].bytes().fold(0f64, |acc, ch| acc * 256.0 + ch as f64);
                out.push(Tok::Num(v));
                i += 1 + end + 2;
            } else if lower.starts_with("pi") {
                out.push(Tok::Num(std::f64::consts::PI));
                i += 3;
            } else if lower.starts_with("phi") {
                out.push(Tok::Num(std::f64::consts::GOLDEN_RATIO));
                i += 4;
            } else if lower.starts_with('e') {
                out.push(Tok::Num(std::f64::consts::E));
                i += 2;
            } else if lower.starts_with('x') {
                let s = i + 2;
                i = s;
                while i < b.len() && b[i].is_ascii_hexdigit() {
                    i += 1;
                }
                out.push(Tok::Num(i64::from_str_radix(&src[s..i], 16).map_err(|e| e.to_string())? as f64));
            } else {
                return Err(format!("unknown constant ${}", rest.chars().take(8).collect::<String>()));
            }
        } else if c == b'"' {
            let end = src[i + 1..].find('"').ok_or("unterminated string")?;
            out.push(Tok::Str(src[i + 1..i + 1 + end].to_owned()));
            i += end + 2;
        } else if c.is_ascii_alphabetic() || c == b'_' || c == b'#' {
            let s = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'.' || b[i] == b'#') {
                i += 1;
            }
            out.push(Tok::Ident(src[s..i].to_ascii_lowercase()));
        } else if let Some(sym) = SYMBOLS.iter().find(|s| src[i..].starts_with(**s)) {
            out.push(Tok::Sym(sym));
            i += sym.len();
        } else {
            out.push(Tok::Sym(match c {
                b'(' => "(",
                b')' => ")",
                b'[' => "[",
                b']' => "]",
                b',' => ",",
                b';' => ";",
                b'{' => "{",
                b'}' => "}",
                b'@' => "@",
                _ => return Err(format!("unexpected character {:?}", c as char)),
            }));
            i += 1;
        }
    }
    Ok(out)
}

// ---- parser (to an unresolved tree) --------------------------------------------------------

#[derive(Debug, Clone)]
enum Ast {
    Num(f64),
    Str(String),
    Name(String),
    Index(Box<Ast>, Box<Ast>),
    Assign(Option<Op>, Box<Ast>, Box<Ast>),
    Bin(Op, Box<Ast>, Box<Ast>),
    Neg(Box<Ast>),
    Not(Box<Ast>),
    AndAlso(Box<Ast>, Box<Ast>),
    OrElse(Box<Ast>, Box<Ast>),
    Cond(Box<Ast>, Box<Ast>, Option<Box<Ast>>),
    Seq(Vec<Ast>),
    Call(String, Vec<Ast>),
}

#[derive(Debug, Clone)]
pub struct Function {
    params: Vec<String>,
    locals: Vec<String>,
    instance: Vec<String>,
    body: Ast,
}

struct Parser {
    toks: Vec<Tok>,
    at: usize,
    functions: HashMap<String, Function>,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.at)
    }

    fn sym(&self, s: &str) -> bool {
        matches!(self.peek(), Some(Tok::Sym(x)) if *x == s)
    }

    fn eat(&mut self, s: &str) -> bool {
        let yes = self.sym(s);
        if yes {
            self.at += 1;
        }
        yes
    }

    fn expect(&mut self, s: &str) -> Result<(), String> {
        if self.eat(s) { Ok(()) } else { Err(format!("expected `{s}`, found {:?}", self.peek())) }
    }

    /// Statements separated by `;` until `end` (or the end of input).
    fn sequence(&mut self, end: Option<&str>) -> Result<Ast, String> {
        let mut items = Vec::new();
        loop {
            while self.eat(";") {}
            if self.peek().is_none() || end.is_some_and(|e| self.sym(e)) {
                break;
            }
            if matches!(self.peek(), Some(Tok::Ident(n)) if n == "function") {
                self.function()?;
                continue;
            }
            items.push(self.assignment()?);
            if !self.sym(";") && !(self.peek().is_none() || end.is_some_and(|e| self.sym(e))) {
                return Err(format!("expected `;`, found {:?}", self.peek()));
            }
        }
        Ok(if items.len() == 1 { items.pop().unwrap() } else { Ast::Seq(items) })
    }

    fn function(&mut self) -> Result<(), String> {
        self.at += 1;
        let Some(Tok::Ident(name)) = self.peek().cloned() else { return Err("function without a name".into()) };
        self.at += 1;
        let names = |p: &mut Parser| -> Result<Vec<String>, String> {
            p.expect("(")?;
            let mut out = Vec::new();
            while !p.eat(")") {
                match p.peek().cloned() {
                    Some(Tok::Ident(n)) => {
                        out.push(n.trim_end_matches('*').to_owned());
                        p.at += 1;
                    }
                    Some(Tok::Sym(",")) => p.at += 1,
                    // `x*` marks a namespace parameter; the star lexes as a symbol.
                    Some(Tok::Sym("*")) => p.at += 1,
                    other => return Err(format!("bad parameter list near {other:?}")),
                }
            }
            Ok(out)
        };
        let params = names(self)?;
        let (mut locals, mut instance) = (Vec::new(), Vec::new());
        while let Some(Tok::Ident(kw)) = self.peek().cloned() {
            match kw.as_str() {
                "local" => {
                    self.at += 1;
                    locals.extend(names(self)?);
                }
                "instance" => {
                    self.at += 1;
                    instance.extend(names(self)?);
                }
                "global" | "globals" => {
                    self.at += 1;
                    names(self)?;
                }
                _ => break,
            }
        }
        self.expect("(")?;
        let body = self.sequence(Some(")"))?;
        self.expect(")")?;
        self.functions.insert(name, Function { params, locals, instance, body });
        Ok(())
    }

    fn assignment(&mut self) -> Result<Ast, String> {
        let lhs = self.ternary()?;
        let op = match self.peek() {
            Some(Tok::Sym("=")) => None,
            Some(Tok::Sym(s)) => match *s {
                "+=" => Some(Op::Add),
                "-=" => Some(Op::Sub),
                "*=" => Some(Op::Mul),
                "/=" => Some(Op::Div),
                "%=" => Some(Op::Mod),
                "^=" => Some(Op::Pow),
                "|=" => Some(Op::Or),
                "&=" => Some(Op::And),
                "~=" => Some(Op::Xor),
                "<<=" => Some(Op::Shl),
                ">>=" => Some(Op::Shr),
                _ => return Ok(lhs),
            },
            _ => return Ok(lhs),
        };
        self.at += 1;
        let rhs = self.assignment()?;
        Ok(Ast::Assign(op, Box::new(lhs), Box::new(rhs)))
    }

    fn ternary(&mut self) -> Result<Ast, String> {
        let cond = self.logic()?;
        if !self.eat("?") {
            return Ok(cond);
        }
        let yes = self.assignment()?;
        let no = if self.eat(":") { Some(Box::new(self.assignment()?)) } else { None };
        Ok(Ast::Cond(Box::new(cond), Box::new(yes), no))
    }

    fn logic(&mut self) -> Result<Ast, String> {
        let mut lhs = self.compare()?;
        loop {
            if self.eat("&&") {
                lhs = Ast::AndAlso(Box::new(lhs), Box::new(self.compare()?));
            } else if self.eat("||") {
                lhs = Ast::OrElse(Box::new(lhs), Box::new(self.compare()?));
            } else {
                return Ok(lhs);
            }
        }
    }

    fn binary(&mut self, ops: &[(&str, Op)], next: fn(&mut Parser) -> Result<Ast, String>) -> Result<Ast, String> {
        let mut lhs = next(self)?;
        'outer: loop {
            for (s, op) in ops {
                if self.eat(s) {
                    lhs = Ast::Bin(*op, Box::new(lhs), Box::new(next(self)?));
                    continue 'outer;
                }
            }
            return Ok(lhs);
        }
    }

    fn compare(&mut self) -> Result<Ast, String> {
        use Op::*;
        self.binary(
            &[("===", ExactEq), ("!==", ExactNe), ("==", Eq), ("!=", Ne), ("<=", Le), (">=", Ge), ("<", Lt), (">", Gt)],
            Parser::bitwise,
        )
    }

    fn bitwise(&mut self) -> Result<Ast, String> {
        self.binary(&[("|", Op::Or), ("&", Op::And), ("~", Op::Xor)], Parser::additive)
    }

    fn additive(&mut self) -> Result<Ast, String> {
        self.binary(&[("+", Op::Add), ("-", Op::Sub)], Parser::shift)
    }

    fn shift(&mut self) -> Result<Ast, String> {
        self.binary(&[("<<", Op::Shl), (">>", Op::Shr)], Parser::multiplicative)
    }

    fn multiplicative(&mut self) -> Result<Ast, String> {
        self.binary(&[("*", Op::Mul), ("/", Op::Div), ("%", Op::Mod)], Parser::power)
    }

    fn power(&mut self) -> Result<Ast, String> {
        let base = self.unary()?;
        if self.eat("^") {
            return Ok(Ast::Bin(Op::Pow, Box::new(base), Box::new(self.power()?)));
        }
        Ok(base)
    }

    fn unary(&mut self) -> Result<Ast, String> {
        if self.eat("-") {
            return Ok(Ast::Neg(Box::new(self.unary()?)));
        }
        if self.eat("+") {
            return self.unary();
        }
        if self.eat("!") {
            return Ok(Ast::Not(Box::new(self.unary()?)));
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Result<Ast, String> {
        let mut e = self.primary()?;
        while self.eat("[") {
            let idx = if self.sym("]") { Ast::Num(0.0) } else { self.sequence(Some("]"))? };
            self.expect("]")?;
            e = Ast::Index(Box::new(e), Box::new(idx));
        }
        Ok(e)
    }

    fn primary(&mut self) -> Result<Ast, String> {
        match self.peek().cloned() {
            Some(Tok::Num(v)) => {
                self.at += 1;
                Ok(Ast::Num(v))
            }
            Some(Tok::Str(s)) => {
                self.at += 1;
                Ok(Ast::Str(s))
            }
            Some(Tok::Sym("(")) => {
                self.at += 1;
                let e = self.sequence(Some(")"))?;
                self.expect(")")?;
                Ok(e)
            }
            Some(Tok::Ident(name)) => {
                self.at += 1;
                if self.eat("(") {
                    let mut args = Vec::new();
                    while !self.eat(")") {
                        args.push(self.sequence_arg()?);
                        if !self.sym(")") {
                            self.expect(",")?;
                        }
                    }
                    // `while (condition) (body)`: the body runs while the condition holds.
                    if name == "while" && args.len() == 1 && self.eat("(") {
                        let body = self.sequence(Some(")"))?;
                        self.expect(")")?;
                        let cond = args.pop().unwrap();
                        return Ok(Ast::Call(
                            name,
                            vec![Ast::Cond(Box::new(cond), Box::new(Ast::Seq(vec![body, Ast::Num(1.0)])), None)],
                        ));
                    }
                    return Ok(Ast::Call(name, args));
                }
                Ok(Ast::Name(name))
            }
            other => Err(format!("unexpected {other:?}")),
        }
    }

    /// A call argument: statements separated by `;` until `,` or `)`.
    fn sequence_arg(&mut self) -> Result<Ast, String> {
        let mut items = vec![self.assignment()?];
        while self.eat(";") {
            if self.sym(",") || self.sym(")") {
                break;
            }
            items.push(self.assignment()?);
        }
        Ok(if items.len() == 1 { items.pop().unwrap() } else { Ast::Seq(items) })
    }
}

// ---- resolution ----------------------------------------------------------------------------

/// Variables by name, shared by every section of an effect.
#[derive(Debug, Default)]
pub struct Vars {
    index: HashMap<String, usize>,
    pub values: Vec<f64>,
    /// String literals and `#named` strings, by handle.
    strings: HashMap<i64, String>,
    named: HashMap<String, i64>,
}

/// Handles of literals and named strings; 0-1023 are the numbered string slots.
const LITERAL_BASE: i64 = 10_000;
const NAMED_BASE: i64 = 90_000;

impl Vars {
    pub fn slot(&mut self, name: &str) -> usize {
        if let Some(&i) = self.index.get(name) {
            return i;
        }
        self.values.push(0.0);
        self.index.insert(name.to_owned(), self.values.len() - 1);
        self.values.len() - 1
    }

    pub fn get(&self, name: &str) -> Option<f64> {
        self.index.get(name).map(|&i| self.values[i])
    }

    fn literal(&mut self, s: &str) -> i64 {
        let handle =
            LITERAL_BASE + self.strings.keys().filter(|k| (LITERAL_BASE..NAMED_BASE).contains(*k)).count() as i64;
        self.strings.insert(handle, s.to_owned());
        handle
    }

    fn named(&mut self, name: &str) -> i64 {
        if let Some(&h) = self.named.get(name) {
            return h;
        }
        let h = NAMED_BASE + self.named.len() as i64;
        self.named.insert(name.to_owned(), h);
        self.strings.insert(h, String::new());
        h
    }
}

/// How names inside an inlined function map to variables.
#[derive(Clone, Default)]
struct Scope {
    renames: HashMap<String, String>,
    /// The namespace `this.` refers to.
    this: Option<String>,
    depth: usize,
}

struct Resolver<'a> {
    vars: &'a mut Vars,
    functions: &'a HashMap<String, Function>,
}

fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some(o) => out.push(o),
            None => {}
        }
    }
    out
}

impl Resolver<'_> {
    fn name(&mut self, scope: &Scope, name: &str) -> usize {
        if let Some(rest) = name.strip_prefix("this.") {
            let full = match &scope.this {
                Some(ns) => format!("{ns}.{rest}"),
                None => rest.to_owned(),
            };
            return self.vars.slot(&full);
        }
        if let Some(r) = scope.renames.get(name) {
            return self.vars.slot(&r.clone());
        }
        self.vars.slot(name)
    }

    fn resolve(&mut self, scope: &Scope, ast: &Ast) -> Result<Node, String> {
        Ok(match ast {
            Ast::Num(v) => Node::Num(*v),
            Ast::Str(s) => Node::Num(self.vars.literal(&unescape(s)) as f64),
            Ast::Name(n) if n.starts_with('#') => Node::Num(self.vars.named(n) as f64),
            Ast::Name(n) => Node::Var(self.name(scope, n)),
            Ast::Index(base, idx) => {
                let base = match &**base {
                    Ast::Name(n) if n == "gmem" => Node::Num(0.0),
                    other => self.resolve(scope, other)?,
                };
                Node::Mem(Box::new(base), Box::new(self.resolve(scope, idx)?))
            }
            // `#name = "…"` copies into a named string, `#name += "…"` appends.
            Ast::Assign(op @ (None | Some(Op::Add)), lhs, rhs) if matches!(&**lhs, Ast::Name(n) if n.starts_with('#')) =>
            {
                let Ast::Name(n) = &**lhs else { unreachable!() };
                let handle = Node::Num(self.vars.named(n) as f64);
                let f = if op.is_some() { Builtin::Strcat } else { Builtin::Strcpy };
                Node::Call(f, vec![handle, self.resolve(scope, rhs)?])
            }
            Ast::Assign(op, lhs, rhs) => {
                let target = self.resolve(scope, lhs)?;
                if !matches!(target, Node::Var(_) | Node::Mem(..) | Node::Call(Builtin::Spl | Builtin::Slider, _)) {
                    return Err("assignment to something that is not a variable".into());
                }
                let rhs = Box::new(self.resolve(scope, rhs)?);
                match op {
                    None => Node::Assign(Box::new(target), rhs),
                    Some(op) => Node::OpAssign(*op, Box::new(target), rhs),
                }
            }
            Ast::Bin(op, a, b) => Node::Bin(*op, Box::new(self.resolve(scope, a)?), Box::new(self.resolve(scope, b)?)),
            Ast::Neg(a) => Node::Neg(Box::new(self.resolve(scope, a)?)),
            Ast::Not(a) => Node::Not(Box::new(self.resolve(scope, a)?)),
            Ast::AndAlso(a, b) => Node::AndAlso(Box::new(self.resolve(scope, a)?), Box::new(self.resolve(scope, b)?)),
            Ast::OrElse(a, b) => Node::OrElse(Box::new(self.resolve(scope, a)?), Box::new(self.resolve(scope, b)?)),
            Ast::Cond(c, y, n) => Node::Cond(
                Box::new(self.resolve(scope, c)?),
                Box::new(self.resolve(scope, y)?),
                n.as_ref().map(|n| self.resolve(scope, n)).transpose()?.map(Box::new),
            ),
            Ast::Seq(items) => Node::Seq(items.iter().map(|i| self.resolve(scope, i)).collect::<Result<_, _>>()?),
            Ast::Call(name, args) => self.call(scope, name, args)?,
        })
    }

    fn call(&mut self, scope: &Scope, name: &str, args: &[Ast]) -> Result<Node, String> {
        match name {
            "while" if args.len() == 1 => return Ok(Node::While(Box::new(self.resolve(scope, &args[0])?))),
            "loop" if args.len() == 2 => {
                return Ok(Node::Loop(
                    Box::new(self.resolve(scope, &args[0])?),
                    Box::new(self.resolve(scope, &args[1])?),
                ));
            }
            _ => {}
        }
        if let Some((b, arity)) = Builtin::from_name(name) {
            if arity != usize::MAX && args.len() != arity {
                return Err(format!("{name}() takes {arity} argument(s)"));
            }
            let args = args.iter().map(|a| self.resolve(scope, a)).collect::<Result<_, _>>()?;
            return Ok(Node::Call(b, args));
        }
        // `ns.fn(...)` calls `fn` with `ns` as its instance namespace.
        // Function names may contain dots themselves, so the namespace is the shortest prefix
        // that leaves a known function.
        let (ns, fname) = match self.functions.get(name) {
            Some(_) => (None, name),
            None => match name
                .match_indices('.')
                .map(|(i, _)| (&name[..i], &name[i + 1..]))
                .find(|(_, f)| self.functions.contains_key(*f))
            {
                Some((ns, f)) => (Some(ns), f),
                None => return Err(format!("unknown function {name}()")),
            },
        };
        let f = self.functions[fname].clone();
        if f.params.len() != args.len() {
            return Err(format!("{fname}() takes {} argument(s)", f.params.len()));
        }
        if scope.depth > 32 {
            return Err(format!("{fname}() nests too deeply (recursion is not allowed)"));
        }
        let this = match ns {
            Some("this") => scope.this.clone(),
            Some(n) => Some(scope.renames.get(n).cloned().unwrap_or_else(|| n.to_owned())),
            None => scope.this.clone(),
        };
        let mut inner = Scope { renames: HashMap::new(), this: this.clone(), depth: scope.depth + 1 };
        for v in f.params.iter().chain(&f.locals) {
            inner.renames.insert(v.clone(), format!("{fname}#{v}"));
        }
        for v in &f.instance {
            let full = match &this {
                Some(ns) => format!("{ns}.{v}"),
                None => v.clone(),
            };
            inner.renames.insert(v.clone(), full);
        }
        let mut seq = Vec::new();
        for (p, a) in f.params.iter().zip(args) {
            let value = self.resolve(scope, a)?;
            seq.push(Node::Assign(Box::new(Node::Var(self.vars.slot(&format!("{fname}#{p}")))), Box::new(value)));
        }
        seq.push(self.resolve(&inner, &f.body)?);
        Ok(Node::Seq(seq))
    }
}

/// A compiled section.
#[derive(Debug, Clone)]
pub struct Code(Node);

/// Compiles `src` against `vars`; functions defined in earlier sections are passed in and the
/// ones defined here are added.
pub fn compile(src: &str, vars: &mut Vars, functions: &mut HashMap<String, Function>) -> Result<Code, String> {
    let mut p = Parser { toks: lex(src)?, at: 0, functions: std::mem::take(functions) };
    let ast = p.sequence(None);
    *functions = p.functions;
    let ast = ast?;
    let mut r = Resolver { vars, functions };
    Ok(Code(r.resolve(&Scope::default(), &ast)?))
}

// ---- interpreter ---------------------------------------------------------------------------

/// What a running effect holds: its variables, memory and the host hooks the builtins reach.
pub struct Machine {
    pub vars: Vars,
    mem: Vec<Option<Box<[f64]>>>,
    rng: u64,
    /// `spl0` … `spl63` and `slider1` … `slider64`, for `spl()` and `slider()`.
    pub spl_slots: [usize; 64],
    pub slider_slots: [usize; 64],
    pub midi_in: Vec<(f64, [u8; 3])>,
    pub midi_out: Vec<(f64, [u8; 3])>,
    midi_read: usize,
    /// What `printf` wrote.
    pub output: String,
}

impl Machine {
    pub fn new(mut vars: Vars) -> Self {
        let spl_slots = std::array::from_fn(|i| vars.slot(&format!("spl{i}")));
        let slider_slots = std::array::from_fn(|i| vars.slot(&format!("slider{}", i + 1)));
        Self {
            vars,
            mem: vec![None; MEM_SIZE / BLOCK],
            rng: 0x2545_f491_4f6c_dd1d,
            spl_slots,
            slider_slots,
            midi_in: Vec::new(),
            midi_out: Vec::new(),
            midi_read: 0,
            output: String::new(),
        }
    }

    fn string(&self, handle: f64) -> String {
        self.vars.strings.get(&(handle.round() as i64)).cloned().unwrap_or_default()
    }

    fn set_string(&mut self, handle: f64, s: String) {
        let h = handle.round() as i64;
        // Literals are constants; the numbered slots and named strings can be written.
        if (0..1024).contains(&h) || h >= NAMED_BASE {
            self.vars.strings.insert(h, s);
        }
    }

    /// C `printf` formatting over EEL values; `%s` takes a string handle.
    fn format(&mut self, args: &[Node]) -> String {
        let fmt = args.first().map_or(String::new(), |a| {
            let h = self.eval(a);
            self.string(h)
        });
        let mut next = 1;
        let mut out = String::new();
        let mut chars = fmt.chars().peekable();
        while let Some(c) = chars.next() {
            if c != '%' {
                out.push(c);
                continue;
            }
            let mut spec = String::new();
            while let Some(&p) = chars.peek() {
                if p.is_ascii_digit() || "-+ #.".contains(p) {
                    spec.push(p);
                    chars.next();
                } else {
                    break;
                }
            }
            let Some(conv) = chars.next() else { break };
            if conv == '%' {
                out.push('%');
                continue;
            }
            let v = args.get(next).map_or(0.0, |a| self.eval(a));
            next += 1;
            let left = spec.starts_with('-');
            let zero = spec.trim_start_matches(['-', '+', ' ', '#']).starts_with('0');
            let (width, prec) = {
                let digits = spec.trim_start_matches(['-', '+', ' ', '#']);
                let (w, p) = digits.split_once('.').map_or((digits, None), |(w, p)| (w, Some(p)));
                (w.parse::<usize>().unwrap_or(0), p.map(|p| p.parse::<usize>().unwrap_or(0)))
            };
            let body = match conv {
                'd' | 'i' | 'u' => format!("{}", v as i64),
                'x' => format!("{:x}", v as i64),
                'X' => format!("{:X}", v as i64),
                'c' => char::from_u32(v as u32).map(String::from).unwrap_or_default(),
                's' => self.string(v),
                'e' => format!("{:.*e}", prec.unwrap_or(6), v),
                'g' => {
                    let s = format!("{:.*}", prec.unwrap_or(6), v);
                    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_owned() } else { s }
                }
                _ => format!("{:.*}", prec.unwrap_or(6), v),
            };
            let pad = width.saturating_sub(body.chars().count());
            if left {
                out.push_str(&body);
                out.extend(std::iter::repeat_n(' ', pad));
            } else if zero && !matches!(conv, 's' | 'c') {
                let (sign, digits) = body.strip_prefix('-').map_or(("", body.as_str()), |d| ("-", d));
                out.push_str(sign);
                out.extend(std::iter::repeat_n('0', pad));
                out.push_str(digits);
            } else {
                out.extend(std::iter::repeat_n(' ', pad));
                out.push_str(&body);
            }
        }
        out
    }

    /// Clears what MIDI the previous block left and queues `events` for `midirecv`.
    pub fn set_midi(&mut self, events: impl Iterator<Item = (f64, [u8; 3])>) {
        self.midi_in.clear();
        self.midi_in.extend(events);
        self.midi_read = 0;
        self.midi_out.clear();
    }

    fn mem_index(x: f64) -> Option<usize> {
        let i = (x + CLOSE).floor();
        (0.0..MEM_SIZE as f64).contains(&i).then_some(i as usize)
    }

    fn read(&self, at: f64) -> f64 {
        match Self::mem_index(at) {
            Some(i) => self.mem[i / BLOCK].as_ref().map_or(0.0, |b| b[i % BLOCK]),
            None => 0.0,
        }
    }

    fn cell(&mut self, at: f64) -> Option<&mut f64> {
        let i = Self::mem_index(at)?;
        let block = self.mem[i / BLOCK].get_or_insert_with(|| vec![0.0; BLOCK].into_boxed_slice());
        Some(&mut block[i % BLOCK])
    }

    pub fn run(&mut self, code: &Code) {
        self.eval(&code.0);
    }

    fn store(&mut self, target: &Node, v: f64) -> f64 {
        match target {
            Node::Var(i) => self.vars.values[*i] = v,
            Node::Call(b @ (Builtin::Spl | Builtin::Slider), args) => {
                let i = args.first().map_or(0.0, |a| self.eval(a)) as usize;
                let slot = match b {
                    Builtin::Spl => self.spl_slots.get(i),
                    _ => i.checked_sub(1).and_then(|i| self.slider_slots.get(i)),
                };
                if let Some(&s) = slot {
                    self.vars.values[s] = v;
                }
            }
            Node::Mem(base, idx) => {
                let at = self.eval(base) + self.eval(idx);
                if let Some(c) = self.cell(at) {
                    *c = v;
                }
            }
            _ => {}
        }
        v
    }

    fn eval(&mut self, n: &Node) -> f64 {
        match n {
            Node::Num(v) => *v,
            Node::Var(i) => self.vars.values[*i],
            Node::Mem(base, idx) => {
                let at = self.eval(base) + self.eval(idx);
                self.read(at)
            }
            Node::Assign(t, v) => {
                let v = self.eval(v);
                self.store(t, v)
            }
            Node::OpAssign(op, t, v) => {
                let rhs = self.eval(v);
                // The target's address is evaluated once, as EEL2 does.
                match &**t {
                    Node::Var(i) => {
                        let r = binop(*op, self.vars.values[*i], rhs);
                        self.vars.values[*i] = r;
                        r
                    }
                    Node::Mem(base, idx) => {
                        let at = self.eval(base) + self.eval(idx);
                        let r = binop(*op, self.read(at), rhs);
                        if let Some(c) = self.cell(at) {
                            *c = r;
                        }
                        r
                    }
                    other => {
                        let r = binop(*op, self.eval(other), rhs);
                        self.store(other, r)
                    }
                }
            }
            Node::Bin(op, a, b) => {
                let a = self.eval(a);
                let b = self.eval(b);
                binop(*op, a, b)
            }
            Node::Neg(a) => -self.eval(a),
            Node::Not(a) => (!truth(self.eval(a))) as u8 as f64,
            Node::AndAlso(a, b) => (truth(self.eval(a)) && truth(self.eval(b))) as u8 as f64,
            Node::OrElse(a, b) => (truth(self.eval(a)) || truth(self.eval(b))) as u8 as f64,
            Node::Cond(c, y, n) => {
                if truth(self.eval(c)) {
                    self.eval(y)
                } else {
                    n.as_ref().map_or(0.0, |n| self.eval(n))
                }
            }
            Node::Seq(items) => {
                let mut v = 0.0;
                for i in items {
                    v = self.eval(i);
                }
                v
            }
            Node::While(body) => {
                // EEL2 stops runaway loops after a million passes.
                let mut n = 0;
                while truth(self.eval(body)) && n < 1_000_000 {
                    n += 1;
                }
                0.0
            }
            Node::Loop(count, body) => {
                let n = self.eval(count).clamp(0.0, 1_000_000.0) as usize;
                for _ in 0..n {
                    self.eval(body);
                }
                0.0
            }
            Node::Call(b, args) => self.builtin(*b, args),
        }
    }

    fn builtin(&mut self, b: Builtin, args: &[Node]) -> f64 {
        use Builtin::*;
        let arg = |m: &mut Self, i: usize| args.get(i).map_or(0.0, |a| m.eval(a));
        match b {
            Sin => arg(self, 0).sin(),
            Cos => arg(self, 0).cos(),
            Tan => arg(self, 0).tan(),
            Asin => arg(self, 0).asin(),
            Acos => arg(self, 0).acos(),
            Atan => arg(self, 0).atan(),
            Atan2 => {
                let y = arg(self, 0);
                y.atan2(arg(self, 1))
            }
            Sqr => {
                let x = arg(self, 0);
                x * x
            }
            Sqrt => arg(self, 0).abs().sqrt(),
            Pow => {
                let x = arg(self, 0);
                x.powf(arg(self, 1))
            }
            Exp => arg(self, 0).exp(),
            Log => arg(self, 0).ln(),
            Log10 => arg(self, 0).log10(),
            Abs => arg(self, 0).abs(),
            Min => {
                let a = arg(self, 0);
                a.min(arg(self, 1))
            }
            Max => {
                let a = arg(self, 0);
                a.max(arg(self, 1))
            }
            Sign => {
                let x = arg(self, 0);
                if x > 0.0 {
                    1.0
                } else if x < 0.0 {
                    -1.0
                } else {
                    0.0
                }
            }
            Rand => {
                let x = arg(self, 0);
                self.rng ^= self.rng << 13;
                self.rng ^= self.rng >> 7;
                self.rng ^= self.rng << 17;
                (self.rng >> 11) as f64 / (1u64 << 53) as f64 * if x < 1.0 { 1.0 } else { x }
            }
            Floor => arg(self, 0).floor(),
            Ceil => arg(self, 0).ceil(),
            Invsqrt => 1.0 / arg(self, 0).abs().sqrt(),
            Memset => {
                let (at, v, n) = (arg(self, 0), arg(self, 1), arg(self, 2));
                for k in 0..(n.max(0.0) as usize).min(MEM_SIZE) {
                    if let Some(c) = self.cell(at + k as f64) {
                        *c = v;
                    }
                }
                at
            }
            Memcpy => {
                let (dst, src, n) = (arg(self, 0), arg(self, 1), arg(self, 2));
                let n = (n.max(0.0) as usize).min(MEM_SIZE);
                let values: Vec<f64> = (0..n).map(|k| self.read(src + k as f64)).collect();
                for (k, v) in values.into_iter().enumerate() {
                    if let Some(c) = self.cell(dst + k as f64) {
                        *c = v;
                    }
                }
                dst
            }
            Freembuf => arg(self, 0),
            Spl => {
                let i = arg(self, 0) as usize;
                self.spl_slots.get(i).map_or(0.0, |&s| self.vars.values[s])
            }
            Slider => {
                let i = arg(self, 0) as usize;
                i.checked_sub(1).and_then(|i| self.slider_slots.get(i)).map_or(0.0, |&s| self.vars.values[s])
            }
            MidiRecv => {
                let Some(&(offset, msg)) = self.midi_in.get(self.midi_read) else { return 0.0 };
                self.midi_read += 1;
                let targets: &[f64] = if args.len() == 3 {
                    &[offset, msg[0] as f64, msg[1] as f64 + msg[2] as f64 * 256.0]
                } else {
                    &[offset, msg[0] as f64, msg[1] as f64, msg[2] as f64]
                };
                for (t, &v) in args.iter().zip(targets) {
                    self.store(t, v);
                }
                1.0
            }
            MidiSend => {
                let (offset, a, b) = (arg(self, 0), arg(self, 1), arg(self, 2));
                let (b, c) = if args.len() == 3 { (b % 256.0, (b / 256.0).floor()) } else { (b, arg(self, 3)) };
                self.midi_out.push((offset, [a as u8, b as u8, c as u8]));
                a
            }
            Printf => {
                let s = self.format(args);
                self.output.push_str(&s);
                s.len() as f64
            }
            Sprintf => {
                let dst = arg(self, 0);
                let s = self.format(&args[1.min(args.len())..]);
                self.set_string(dst, s);
                dst
            }
            Strcpy | Strcat => {
                let (dst, src) = (arg(self, 0), arg(self, 1));
                let mut s = if b == Strcat { self.string(dst) } else { String::new() };
                s.push_str(&self.string(src));
                self.set_string(dst, s);
                dst
            }
            Strlen => {
                let h = arg(self, 0);
                self.string(h).len() as f64
            }
            Strcmp | Stricmp => {
                let (a, c) = (arg(self, 0), arg(self, 1));
                let (mut a, mut c) = (self.string(a), self.string(c));
                if b == Stricmp {
                    (a, c) = (a.to_lowercase(), c.to_lowercase());
                }
                match a.cmp(&c) {
                    std::cmp::Ordering::Less => -1.0,
                    std::cmp::Ordering::Equal => 0.0,
                    std::cmp::Ordering::Greater => 1.0,
                }
            }
            StrGetchar => {
                let (h, i) = (arg(self, 0), arg(self, 1));
                let s = self.string(h);
                let i = if i < 0.0 { s.len() as f64 + i } else { i };
                s.as_bytes().get(i.max(0.0) as usize).map_or(0.0, |&c| c as f64)
            }
            MemTop => MEM_SIZE as f64,
            TimePrecise => {
                let t =
                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
                if let Some(target) = args.first() {
                    self.store(target, t);
                }
                t
            }
            NoChange => {
                arg(self, 0);
                arg(self, 1);
                -1.0
            }
            Nop => {
                for i in 0..args.len() {
                    arg(self, i);
                }
                0.0
            }
        }
    }
}

fn truth(x: f64) -> bool {
    x.abs() >= CLOSE
}

fn int(x: f64) -> i64 {
    x as i64
}

fn binop(op: Op, a: f64, b: f64) -> f64 {
    use Op::*;
    match op {
        Add => a + b,
        Sub => a - b,
        Mul => a * b,
        Div => a / b,
        Mod => {
            let d = int(b.abs());
            if d == 0 { 0.0 } else { (int(a.abs()) % d) as f64 }
        }
        Pow => a.powf(b),
        Or => (int(a) | int(b)) as f64,
        And => (int(a) & int(b)) as f64,
        Xor => (int(a) ^ int(b)) as f64,
        Shl => ((int(a) as i32).wrapping_shl(int(b) as u32)) as f64,
        Shr => ((int(a) as i32).wrapping_shr(int(b) as u32)) as f64,
        Eq => ((a - b).abs() < CLOSE) as u8 as f64,
        ExactEq => (a == b) as u8 as f64,
        Ne => ((a - b).abs() >= CLOSE) as u8 as f64,
        ExactNe => (a != b) as u8 as f64,
        Lt => (a < b) as u8 as f64,
        Gt => (a > b) as u8 as f64,
        Le => (a <= b) as u8 as f64,
        Ge => (a >= b) as u8 as f64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(src: &str) -> Machine {
        let mut vars = Vars::default();
        let mut fns = HashMap::new();
        let code = compile(src, &mut vars, &mut fns).unwrap();
        let mut m = Machine::new(vars);
        m.run(&code);
        m
    }

    fn var(m: &Machine, n: &str) -> f64 {
        m.vars.get(n).unwrap()
    }

    #[test]
    fn expressions_and_precedence() {
        let m =
            run("a = 1 + 2 * 3 ^ 2; b = -2 ^ 2; c = 7 % 3; d = (1; 2; 3); e = 5 | 2; f = 1 << 3 + 1; g = 0x10 + $'A';");
        assert_eq!(var(&m, "a"), 19.0);
        assert_eq!(var(&m, "b"), 4.0, "unary minus binds tighter than ^");
        assert_eq!((var(&m, "c"), var(&m, "d"), var(&m, "e")), (1.0, 3.0, 7.0));
        assert_eq!(var(&m, "f"), 9.0, "<< binds tighter than +");
        assert_eq!(var(&m, "g"), 16.0 + 65.0);
    }

    #[test]
    fn conditions_loops_and_memory() {
        let m = run("x = 0.000001 ? 1 : 2; y = 1 == 1.000001; i = 0; s = 0; while (s += i; i += 1; i < 5); \
             buf = 100; loop(4, buf[k] = k * 2; k += 1); t = buf[3]; q = 3 > 2 ? (z = 5; 6) : 7; \
             a.b = 2; a.b *= 4;");
        assert_eq!(var(&m, "x"), 2.0, "values within 1e-5 of zero are false");
        assert_eq!(var(&m, "y"), 1.0, "== tolerates 1e-5");
        assert_eq!(var(&m, "s"), 10.0);
        assert_eq!((var(&m, "t"), var(&m, "q"), var(&m, "z")), (6.0, 6.0, 5.0));
        assert_eq!(var(&m, "a.b"), 8.0);
    }

    #[test]
    fn functions_with_locals_and_instances() {
        let m = run("function lp(x) instance(state) local(k) ( k = 0.5; state += k * (x - state) ); \
             function twice(v) ( v * 2 ); \
             f1.lp(10); f1.lp(10); f2.lp(4); r = twice(f1.state); k = 9;");
        assert_eq!(var(&m, "f1.state"), 7.5);
        assert_eq!(var(&m, "f2.state"), 2.0);
        assert_eq!(var(&m, "r"), 15.0);
        assert_eq!(var(&m, "k"), 9.0, "locals do not touch globals");
    }

    #[test]
    fn while_with_a_body_dotted_functions_and_spl_targets() {
        let m = run("i = 0; n = 0; while (i < 4) ( n += 2; i += 1; ); \
             function parmchg.set(v) instance(x) ( x = v ); gain.parmchg.set(3); \
             spl0 = 1; spl(0) = 5; spl(0) += 1;");
        assert_eq!((var(&m, "i"), var(&m, "n")), (4.0, 8.0));
        assert_eq!(var(&m, "gain.x"), 3.0);
        assert_eq!(var(&m, "spl0"), 6.0);
    }

    #[test]
    fn strings_and_printf() {
        let m = run(
            "#s = \"x\"; strcat(#s, \"y\"); n = strlen(#s); printf(\"a%02d|%s|%.2f|%-3d|\\n\", 7, #s, 3.14159, 5); \
                     sprintf(#t, \"%d-%d\", 1, 2); same = strcmp(#t, \"1-2\") == 0;",
        );
        assert_eq!(var(&m, "n"), 2.0);
        assert_eq!(m.output, "a07|xy|3.14|5  |\n");
        assert_eq!(var(&m, "same"), 1.0);
    }
}
