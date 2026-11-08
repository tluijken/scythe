//! EEL2, the expression language of JSFX: a compiler from source to a tree of resolved
//! operations and an interpreter for it. Every value is an f64; variables are global (names
//! are case-insensitive); `x[i]` addresses the shared memory. User functions are inlined at
//! each call with their parameters, locals and instance variables renamed, as EEL2 itself does.

mod files;
pub mod gfx;
mod host;
mod matching;
mod spectral;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

pub use host::{Host, HostArgs, compile_with_host};

/// Values closer than this count as equal, and closer than this to zero as false.
const CLOSE: f64 = 0.00001;
/// Memory slots, as JSFX gives each effect.
pub const MEM_SIZE: usize = 8 * 1024 * 1024;
/// Memory is allocated in blocks of `1 << MEM_SHIFT` slots, variables in blocks of
/// `1 << VAR_SHIFT`.
const MEM_SHIFT: u32 = 16;
const VAR_SHIFT: u32 = 12;
/// Variables an effect can have.
const MAX_VARS: usize = 1 << 22;

/// Numbers shared by the machines of one effect: the audio thread's and its `@gfx` thread's,
/// as REAPER runs `@gfx` on the UI thread over the same variables and memory while audio
/// runs. Each value is an `f64` kept in an atomic, so the threads race as REAPER's do, a value
/// at a time, without undefined behaviour; relaxed loads and stores compile to plain moves.
/// Blocks are allocated on first write, so untouched memory costs nothing.
pub struct Cells {
    blocks: Box<[OnceLock<Box<[AtomicU64]>>]>,
    shift: u32,
}

impl Cells {
    fn new(shift: u32, len: usize) -> Self {
        Self { blocks: (0..len >> shift).map(|_| OnceLock::new()).collect(), shift }
    }

    fn get(&self, i: usize) -> f64 {
        match self.blocks.get(i >> self.shift).and_then(OnceLock::get) {
            Some(b) => f64::from_bits(b[i & ((1 << self.shift) - 1)].load(Ordering::Relaxed)),
            None => 0.0,
        }
    }

    fn set(&self, i: usize, v: f64) {
        if let Some(block) = self.blocks.get(i >> self.shift) {
            let b = block.get_or_init(|| (0..1usize << self.shift).map(|_| AtomicU64::new(0)).collect());
            b[i & ((1 << self.shift) - 1)].store(v.to_bits(), Ordering::Relaxed);
        }
    }
}

impl std::fmt::Debug for Cells {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Cells({} blocks)", self.blocks.len())
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

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
    /// `max` where REAPER compiles it for a pointer result (see [`mark_pointers`]).
    MaxPtr,
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
    MidiRecvBuf,
    MidiSendBuf,
    MidiSyx,
    FileOpen,
    FileClose,
    FileRewind,
    FileRiff,
    FileText,
    FileAvail,
    FileVar,
    FileMem,
    FileString,
    AtomicGet,
    AtomicSet,
    AtomicAdd,
    AtomicExch,
    AtomicSetIfEqual,
    Fft,
    Ifft,
    FftReal,
    IfftReal,
    FftPermute,
    FftIpermute,
    ConvolveC,
    Mdct,
    Imdct,
    /// No more parameter changes queued in this block (`slider_next_chg`).
    NoChange,
    /// Accepted and ignored (UI and host notifications).
    Nop,
    StrcpySubstr,
    StrSetchar,
    StrcpyFromslider,
    /// `match` (`true`) and `matchi` (`false` folds case).
    Match(bool),
    /// The track's channels, as `num_ch` has them.
    HostNumchan,
    /// The pin map, always the identity: pin `n` on channel `n`.
    PinMapping,
    /// `gfx_*` drawing, for `@gfx`.
    Gfx(gfx::Call),
    /// A function the embedder answers, by its index in the names given to
    /// `compile_with_host`.
    Host(usize),
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
            "strcpy_substr" => (StrcpySubstr, usize::MAX),
            "str_setchar" => (StrSetchar, usize::MAX),
            "strcpy_fromslider" => (StrcpyFromslider, 2),
            "match" => (Match(true), usize::MAX),
            "matchi" => (Match(false), usize::MAX),
            "get_host_numchan" => (HostNumchan, 0),
            "get_pin_mapping" => (PinMapping, 4),
            "set_host_numchan" | "set_pin_mapping" | "set_pinmapper_flags" => (Nop, usize::MAX),
            "strcmp" => (Strcmp, 2),
            "stricmp" => (Stricmp, 2),
            "str_getchar" => (StrGetchar, usize::MAX),
            "__memtop" => (MemTop, 0),
            "time_precise" => (TimePrecise, usize::MAX),
            "slider_next_chg" => (NoChange, 2),
            "midirecv_buf" => (MidiRecvBuf, 3),
            "file_open" => (FileOpen, 1),
            "file_close" => (FileClose, 1),
            "file_rewind" => (FileRewind, 1),
            "file_riff" => (FileRiff, 3),
            "file_text" => (FileText, 1),
            // `file_avail()` without a handle reads the `@serialize` stream (handle 0).
            "file_avail" => (FileAvail, usize::MAX),
            "file_var" => (FileVar, 2),
            "file_mem" => (FileMem, 3),
            "file_string" => (FileString, 2),
            "midisend_buf" => (MidiSendBuf, 3),
            "midisyx" => (MidiSyx, 3),
            "atomic_get" => (AtomicGet, 1),
            "atomic_set" => (AtomicSet, 2),
            "atomic_add" => (AtomicAdd, 2),
            "atomic_exch" => (AtomicExch, 2),
            "atomic_setifequal" => (AtomicSetIfEqual, 3),
            "fft" => (Fft, 2),
            "ifft" => (Ifft, 2),
            "fft_real" => (FftReal, 2),
            "ifft_real" => (IfftReal, 2),
            "fft_permute" => (FftPermute, 2),
            "fft_ipermute" => (FftIpermute, 2),
            "convolve_c" => (ConvolveC, 3),
            "mdct" => (Mdct, 2),
            "imdct" => (Imdct, 2),
            "slider_automate" | "sliderchange" | "slider_show" | "export_buffer_to_project" | "get_pinmapper_flags" => {
                (Nop, usize::MAX)
            }
            _ => return gfx::Call::from_name(name).map(|(c, arity)| (Gfx(c), arity)),
        })
    }
}

#[derive(Debug, Clone)]
enum Node {
    Num(f64),
    Var(usize),
    /// `base[index]`; `gmem[index]` uses the shared memory, which here is the same.
    Mem(Box<Node>, Box<Node>),
    /// `target = value`; a checked assignment stores through [`assign_filter`].
    Assign(Box<Node>, Box<Node>, bool),
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
                // EEL2 has no exponents: `1e5`, like any number run into a name, fails to
                // compile (measured in REAPER, which then leaves the section out).
                if b.get(i).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'#') {
                    let end =
                        src[i..].find(|c: char| !c.is_ascii_alphanumeric() && c != '_').map_or(b.len(), |e| i + e);
                    return Err(format!("bad number {}", &src[s..end]));
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
        } else if c == b'\'' {
            // `'ab'` is a number as `$'ab'` is (used for keys and font flags).
            let end = src[i + 1..].find('\'').ok_or("unterminated character")?;
            out.push(Tok::Num(src[i + 1..i + 1 + end].bytes().fold(0f64, |acc, ch| acc * 256.0 + ch as f64)));
            i += end + 2;
        } else if c == b'"' {
            let end = src[i + 1..].find('"').ok_or("unterminated string")?;
            let text = &src[i + 1..i + 1 + end];
            // Adjacent literals join, as in C.
            match out.last_mut() {
                Some(Tok::Str(prev)) => prev.push_str(text),
                _ => out.push(Tok::Str(text.to_owned())),
            }
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
        Ok(assign_rightmost(op, lhs, rhs))
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
            while self.eat(";") {}
            if self.sym(",") || self.sym(")") {
                break;
            }
            items.push(self.assignment()?);
        }
        Ok(if items.len() == 1 { items.pop().unwrap() } else { Ast::Seq(items) })
    }
}

/// EEL2 assigns to the rightmost operand of a logical or binary expression: `a && b = 1` is
/// `a && (b = 1)`.
fn assign_rightmost(op: Option<Op>, lhs: Ast, rhs: Ast) -> Ast {
    match lhs {
        Ast::AndAlso(a, b) => Ast::AndAlso(a, Box::new(assign_rightmost(op, *b, rhs))),
        Ast::OrElse(a, b) => Ast::OrElse(a, Box::new(assign_rightmost(op, *b, rhs))),
        Ast::Bin(o, a, b) => Ast::Bin(o, a, Box::new(assign_rightmost(op, *b, rhs))),
        lhs => Ast::Assign(op, Box::new(lhs), Box::new(rhs)),
    }
}

// ---- resolution ----------------------------------------------------------------------------

/// Variables by name, shared by every section of an effect. Values and strings are shared
/// with every [`Vars::share`] of them.
#[derive(Debug)]
pub struct Vars {
    index: HashMap<String, usize>,
    values: Arc<Cells>,
    /// String literals and `#named` strings, by handle.
    strings: Arc<Mutex<HashMap<i64, String>>>,
    named: HashMap<String, i64>,
}

impl Default for Vars {
    fn default() -> Self {
        Self {
            index: HashMap::new(),
            values: Arc::new(Cells::new(VAR_SHIFT, MAX_VARS)),
            strings: Default::default(),
            named: HashMap::new(),
        }
    }
}

/// Handles of literals and named strings; 0-1023 are the numbered string slots.
const LITERAL_BASE: i64 = 10_000;
const NAMED_BASE: i64 = 90_000;

impl Vars {
    pub fn slot(&mut self, name: &str) -> usize {
        if let Some(&i) = self.index.get(name) {
            return i;
        }
        let i = self.index.len();
        self.index.insert(name.to_owned(), i);
        i
    }

    pub fn get(&self, name: &str) -> Option<f64> {
        self.index.get(name).map(|&i| self.at(i))
    }

    /// The value in slot `i`.
    pub fn at(&self, i: usize) -> f64 {
        self.values.get(i)
    }

    pub fn put(&self, i: usize, v: f64) {
        self.values.set(i, v);
    }

    /// Every variable's value, for [`Vars::restore`].
    pub fn snapshot(&self) -> Vec<f64> {
        (0..self.index.len()).map(|i| self.at(i)).collect()
    }

    pub fn restore(&self, values: &[f64]) {
        for (i, &v) in values.iter().enumerate() {
            self.put(i, v);
        }
    }

    /// The same variables, values and strings, for a machine on another thread.
    pub fn share(&self) -> Self {
        Self {
            index: self.index.clone(),
            values: self.values.clone(),
            strings: self.strings.clone(),
            named: self.named.clone(),
        }
    }

    fn literal(&mut self, s: &str) -> i64 {
        let mut strings = lock(&self.strings);
        let handle = LITERAL_BASE + strings.keys().filter(|k| (LITERAL_BASE..NAMED_BASE).contains(*k)).count() as i64;
        strings.insert(handle, s.to_owned());
        handle
    }

    fn named(&mut self, name: &str) -> i64 {
        if let Some(&h) = self.named.get(name) {
            return h;
        }
        let h = NAMED_BASE + self.named.len() as i64;
        self.named.insert(name.to_owned(), h);
        lock(&self.strings).insert(h, String::new());
        h
    }

    fn string(&self, handle: f64) -> String {
        lock(&self.strings).get(&(handle.round() as i64)).cloned().unwrap_or_default()
    }

    fn set_string(&self, handle: f64, s: String) {
        let h = handle.round() as i64;
        // Literals are constants; the numbered slots and named strings can be written.
        if (0..1024).contains(&h) || h >= NAMED_BASE {
            lock(&self.strings).insert(h, s);
        }
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
    host: &'a [&'a str],
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
                    None => Node::Assign(Box::new(target), rhs, true),
                    Some(op) => Node::OpAssign(*op, Box::new(target), rhs),
                }
            }
            Ast::Bin(op, a, b) => fold_bin(*op, self.resolve(scope, a)?, self.resolve(scope, b)?),
            Ast::Neg(a) => match self.resolve(scope, a)? {
                Node::Num(v) => Node::Num(-v),
                a => Node::Neg(Box::new(a)),
            },
            Ast::Not(a) => match self.resolve(scope, a)? {
                Node::Num(v) => Node::Num((!truth(v)) as u8 as f64),
                a => Node::Not(Box::new(a)),
            },
            Ast::AndAlso(a, b) => match (self.resolve(scope, a)?, self.resolve(scope, b)?) {
                (Node::Num(a), Node::Num(b)) => Node::Num((truth(a) && truth(b)) as u8 as f64),
                (a, b) => Node::AndAlso(Box::new(a), Box::new(b)),
            },
            Ast::OrElse(a, b) => match (self.resolve(scope, a)?, self.resolve(scope, b)?) {
                (Node::Num(a), Node::Num(b)) => Node::Num((truth(a) || truth(b)) as u8 as f64),
                (a, b) => Node::OrElse(Box::new(a), Box::new(b)),
            },
            Ast::Cond(c, y, n) => {
                let c = self.resolve(scope, c)?;
                let y = self.resolve(scope, y)?;
                let n = n.as_ref().map(|n| self.resolve(scope, n)).transpose()?;
                match c {
                    Node::Num(c) if truth(c) => y,
                    Node::Num(_) => n.unwrap_or(Node::Num(0.0)),
                    c => Node::Cond(Box::new(c), Box::new(y), n.map(Box::new)),
                }
            }
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
            let args: Vec<Node> = args.iter().map(|a| self.resolve(scope, a)).collect::<Result<_, _>>()?;
            let consts: Option<Vec<f64>> =
                args.iter().map(|a| if let Node::Num(v) = a { Some(*v) } else { None }).collect();
            if let Some(v) = consts.and_then(|c| folded_math(b, &c)) {
                return Ok(Node::Num(v));
            }
            return Ok(Node::Call(b, args));
        }
        if !self.functions.contains_key(name)
            && let Some(index) = self.host.iter().position(|h| h.eq_ignore_ascii_case(name))
        {
            let args = args.iter().map(|a| self.resolve(scope, a)).collect::<Result<_, _>>()?;
            return Ok(Node::Call(Builtin::Host(index), args));
        }
        // `ns.fn(...)` calls `fn` with `ns` as its instance namespace.
        // Function names may contain dots themselves, so the namespace is the shortest prefix
        // that leaves a known function; the instance namespace is then the whole call name
        // up to its last dot (`a.obj.go()` calling `obj.go` works on `a.obj`).
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
            Some(n) => {
                let n = scope.renames.get(n).cloned().unwrap_or_else(|| n.to_owned());
                Some(match fname.rsplit_once('.') {
                    Some((inner, _)) => format!("{n}.{inner}"),
                    None => n,
                })
            }
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
            // Parameters are bound without the assignment check (measured in REAPER).
            seq.push(Node::Assign(
                Box::new(Node::Var(self.vars.slot(&format!("{fname}#{p}")))),
                Box::new(value),
                false,
            ));
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
    compile_with_host(src, vars, functions, &[])
}

// ---- interpreter ---------------------------------------------------------------------------

/// What a running effect holds: its variables, memory and the host hooks the builtins reach.
pub struct Machine {
    pub vars: Vars,
    mem: Arc<Cells>,
    rng: u64,
    /// `spl0` … `spl63` and `slider1` … `slider64`, for `spl()` and `slider()`.
    pub spl_slots: [usize; 64],
    pub slider_slots: [usize; 64],
    /// The block's incoming messages and whether a `midirecv` took each; the rest pass on.
    pub midi_in: Vec<(f64, MidiMsg, bool)>,
    pub midi_out: Vec<(f64, MidiMsg)>,
    midi_read: usize,
    /// What `printf` wrote.
    pub output: String,
    spectral: spectral::Spectral,
    /// The slots of file sliders and the files each one chooses between.
    pub file_sliders: Vec<(usize, Vec<std::path::PathBuf>)>,
    /// Where `file_open` looks for a file named by a string.
    pub data_dirs: Vec<std::path::PathBuf>,
    files: Vec<Option<files::OpenFile>>,
    /// The `@serialize` stream (file handle 0) while that section runs.
    pub serial: Option<Serial>,
    gfx: gfx::Link,
}

/// Values `@serialize` reads back (`writing` false) or writes.
#[derive(Debug, Default)]
pub struct Serial {
    pub values: Vec<f64>,
    pub pos: usize,
    pub writing: bool,
}

impl Serial {
    fn next(&mut self) -> Option<f64> {
        let v = self.values.get(self.pos).copied()?;
        self.pos += 1;
        Some(v)
    }
}

/// A MIDI message in a JSFX's queue: a channel message, or system exclusive / meta kept whole.
#[derive(Debug, Clone, PartialEq)]
pub enum MidiMsg {
    Short([u8; 3]),
    Long(Vec<u8>),
}

/// Bytes in a channel message with this status byte.
fn midi_len(status: u8) -> usize {
    match status & 0xf0 {
        0xc0 | 0xd0 => 2,
        _ => 3,
    }
}

/// What an expression used as a target writes to: `(x = 0)` stands for `x`.
fn place(node: &Node) -> &Node {
    match node {
        Node::Assign(target, ..) => place(target),
        n => n,
    }
}

impl Machine {
    pub fn new(mut vars: Vars) -> Self {
        let spl_slots = std::array::from_fn(|i| vars.slot(&format!("spl{i}")));
        let slider_slots = std::array::from_fn(|i| vars.slot(&format!("slider{}", i + 1)));
        let gfx = gfx::Link::new(&mut vars);
        Self {
            vars,
            mem: Arc::new(Cells::new(MEM_SHIFT, MEM_SIZE)),
            rng: 0x2545_f491_4f6c_dd1d,
            spl_slots,
            slider_slots,
            midi_in: Vec::new(),
            midi_out: Vec::new(),
            midi_read: 0,
            output: String::new(),
            spectral: Default::default(),
            file_sliders: Vec::new(),
            data_dirs: Vec::new(),
            files: Vec::new(),
            serial: None,
            gfx,
        }
    }

    /// A machine over the same variables, memory, strings and graphics, with files, MIDI and
    /// the rest of its own: what runs `@gfx` beside the audio thread's machine.
    pub fn share(&self) -> Self {
        Self {
            vars: self.vars.share(),
            mem: self.mem.clone(),
            rng: self.rng,
            spl_slots: self.spl_slots,
            slider_slots: self.slider_slots,
            midi_in: Vec::new(),
            midi_out: Vec::new(),
            midi_read: 0,
            output: String::new(),
            spectral: Default::default(),
            file_sliders: self.file_sliders.clone(),
            data_dirs: self.data_dirs.clone(),
            files: Vec::new(),
            serial: None,
            gfx: self.gfx.share(),
        }
    }

    fn string(&self, handle: f64) -> String {
        self.vars.string(handle)
    }

    fn set_string(&mut self, handle: f64, s: String) {
        self.vars.set_string(handle, s);
    }

    /// The string a handle names, as `string` arguments are passed.
    pub fn string_at(&self, handle: f64) -> String {
        self.string(handle)
    }

    /// Writes the string a handle names, if it is writable.
    pub fn set_string_at(&mut self, handle: f64, s: String) {
        self.set_string(handle, s);
    }

    /// The handle of the named string `#name` (given with its `#`), creating it if needed.
    pub fn named_string(&mut self, name: &str) -> f64 {
        self.vars.named(name) as f64
    }

    /// C `printf` formatting over EEL values; `%s` takes a string handle.
    fn format(&mut self, args: &[Node]) -> String {
        let fmt = args.first().map_or(String::new(), |a| {
            let h = self.eval(a);
            self.string(h)
        });
        let mut next = 1;
        self.format_with(&fmt, |m| {
            let v = args.get(next).map_or(0.0, |a| m.eval(a));
            next += 1;
            v
        })
    }

    /// Formats `fmt`, taking each value from `next` as a conversion needs it.
    pub fn format_with(&mut self, fmt: &str, mut next: impl FnMut(&mut Self) -> f64) -> String {
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
            let v = next(self);
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

    /// Clears what MIDI the previous block left and queues `events` for `midirecv`, in frame
    /// order (channel messages before long ones at the same frame).
    pub fn set_midi(&mut self, events: impl Iterator<Item = (f64, MidiMsg)>) {
        self.midi_in.clear();
        self.midi_in.extend(events.map(|(o, m)| (o, m, false)));
        self.midi_in.sort_by(|a, b| {
            a.0.total_cmp(&b.0).then((matches!(a.1, MidiMsg::Long(_))).cmp(&matches!(b.1, MidiMsg::Long(_))))
        });
        self.midi_read = 0;
        self.midi_out.clear();
    }

    /// What the block passes on into `out`, offsets within `n` frames and moved by `base`:
    /// the messages no `midirecv` took, then those sent, in frame order.
    pub fn drain_midi(&mut self, n: usize, base: usize, out: &mut crate::fx::MidiBlock) {
        let (from_short, from_long) = (out.short.len(), out.long.len());
        let last = n.saturating_sub(1) as f64;
        let at = |offset: f64| base + offset.clamp(0.0, last) as usize;
        let kept = self.midi_in.drain(..).filter(|e| !e.2).map(|(o, m, _)| (o, m));
        for (o, m) in kept.chain(self.midi_out.drain(..)) {
            match m {
                MidiMsg::Short(b) => out.short.push((at(o), b)),
                MidiMsg::Long(d) => out.long.push((at(o), d)),
            }
        }
        out.short[from_short..].sort_by_key(|e| e.0);
        out.long[from_long..].sort_by_key(|e| e.0);
        self.midi_read = 0;
    }

    /// The next message `midirecv` (channel messages only) or `midirecv_buf` (any) takes.
    fn next_midi(&self, long_too: bool) -> Option<usize> {
        (self.midi_read..self.midi_in.len())
            .find(|&i| !self.midi_in[i].2 && (long_too || matches!(self.midi_in[i].1, MidiMsg::Short(_))))
    }

    fn file_slot(&mut self, handle: f64) -> Option<&mut Option<files::OpenFile>> {
        let i = handle.round();
        if i < 1.0 {
            return None;
        }
        self.files.get_mut(i as usize - 1)
    }

    fn mem_index(x: f64) -> Option<usize> {
        let i = (x + CLOSE).floor();
        (0.0..MEM_SIZE as f64).contains(&i).then_some(i as usize)
    }

    fn read(&self, at: f64) -> f64 {
        Self::mem_index(at).map_or(0.0, |i| self.mem.get(i))
    }

    fn write(&self, at: f64, v: f64) {
        if let Some(i) = Self::mem_index(at) {
            self.mem.set(i, v);
        }
    }

    /// Runs `code`; its value is that of its last expression.
    pub fn run(&mut self, code: &Code) -> f64 {
        self.eval(&code.0)
    }

    fn store(&mut self, target: &Node, v: f64) -> f64 {
        match target {
            Node::Var(i) => self.vars.put(*i, v),
            Node::Call(b @ (Builtin::Spl | Builtin::Slider), args) => {
                let i = args.first().map_or(0.0, |a| self.eval(a)) as usize;
                let slot = match b {
                    Builtin::Spl => self.spl_slots.get(i),
                    _ => i.checked_sub(1).and_then(|i| self.slider_slots.get(i)),
                };
                if let Some(&s) = slot {
                    self.vars.put(s, v);
                }
            }
            Node::Mem(base, idx) => {
                let at = self.eval(base) + self.eval(idx);
                self.write(at, v);
            }
            _ => {}
        }
        v
    }

    fn eval(&mut self, n: &Node) -> f64 {
        match n {
            Node::Num(v) => *v,
            Node::Var(i) => self.vars.at(*i),
            Node::Mem(base, idx) => {
                let at = self.eval(base) + self.eval(idx);
                self.read(at)
            }
            Node::Assign(t, v, checked) => {
                let v = self.eval(v);
                self.store(t, if *checked { assign_filter(v) } else { v })
            }
            Node::OpAssign(op, t, v) => {
                let rhs = self.eval(v);
                // The target's address is evaluated once, as EEL2 does.
                match &**t {
                    Node::Var(i) => {
                        let r = binop(*op, self.vars.at(*i), rhs);
                        self.vars.put(*i, r);
                        r
                    }
                    Node::Mem(base, idx) => {
                        let at = self.eval(base) + self.eval(idx);
                        let r = binop(*op, self.read(at), rhs);
                        self.write(at, r);
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
                // What REAPER's `loop()` evaluates to, whatever its count or body.
                1.0
            }
            Node::Call(b, args) => self.builtin(*b, args),
        }
    }

    fn builtin(&mut self, b: Builtin, args: &[Node]) -> f64 {
        use Builtin::*;
        let arg = |m: &mut Self, i: usize| args.get(i).map_or(0.0, |a| m.eval(a));
        match b {
            Sin | Cos | Tan | Asin | Acos | Atan | Sqrt | Exp | Log | Log10 | Floor | Ceil | Abs | Sqr | Sign
            | Invsqrt => {
                let x = arg(self, 0);
                math(b, x, 0.0)
            }
            Atan2 | Pow | Min | Max | MaxPtr => {
                let x = arg(self, 0);
                let y = arg(self, 1);
                math(b, x, y)
            }
            Rand => {
                let x = arg(self, 0);
                self.rng ^= self.rng << 13;
                self.rng ^= self.rng >> 7;
                self.rng ^= self.rng << 17;
                (self.rng >> 11) as f64 / (1u64 << 53) as f64 * if x < 1.0 { 1.0 } else { x }
            }
            Memset => {
                let (at, v, n) = (arg(self, 0), arg(self, 1), arg(self, 2));
                for k in 0..(n.max(0.0) as usize).min(MEM_SIZE) {
                    self.write(at + k as f64, v);
                }
                at
            }
            Memcpy => {
                let (dst, src, n) = (arg(self, 0), arg(self, 1), arg(self, 2));
                let n = (n.max(0.0) as usize).min(MEM_SIZE);
                let values: Vec<f64> = (0..n).map(|k| self.read(src + k as f64)).collect();
                for (k, v) in values.into_iter().enumerate() {
                    self.write(dst + k as f64, v);
                }
                dst
            }
            Freembuf => arg(self, 0),
            Spl => {
                let i = arg(self, 0) as usize;
                self.spl_slots.get(i).map_or(0.0, |&s| self.vars.at(s))
            }
            Slider => {
                let i = arg(self, 0) as usize;
                i.checked_sub(1).and_then(|i| self.slider_slots.get(i)).map_or(0.0, |&s| self.vars.at(s))
            }
            MidiRecv => {
                let Some(i) = self.next_midi(false) else { return 0.0 };
                let (offset, MidiMsg::Short(msg), _) = self.midi_in[i] else { return 0.0 };
                self.midi_in[i].2 = true;
                self.midi_read = i + 1;
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
                self.midi_out.push((offset, MidiMsg::Short([a as u8, b as u8, c as u8])));
                a
            }
            MidiRecvBuf => {
                let (at, max) = (arg(self, 1), arg(self, 2));
                let Some(i) = self.next_midi(true) else { return 0.0 };
                let (offset, bytes) = match &self.midi_in[i] {
                    (o, MidiMsg::Short(m), _) => (*o, m[..midi_len(m[0])].to_vec()),
                    (o, MidiMsg::Long(d), _) => (*o, d.clone()),
                };
                if (bytes.len() as f64) > max {
                    return 0.0;
                }
                self.midi_in[i].2 = true;
                self.midi_read = i + 1;
                if let Some(t) = args.first() {
                    self.store(t, offset);
                }
                for (k, &byte) in bytes.iter().enumerate() {
                    self.write(at + k as f64, byte as f64);
                }
                bytes.len() as f64
            }
            MidiSendBuf | MidiSyx => {
                let (offset, at, len) = (arg(self, 0), arg(self, 1), arg(self, 2));
                let len = (len.max(0.0) as usize).min(65536);
                if len == 0 {
                    return 0.0;
                }
                let mut bytes: Vec<u8> = (0..len).map(|k| self.read(at + k as f64) as u8).collect();
                let msg = if matches!(b, MidiSyx) {
                    // The F0 and F7 framing is optional for midisyx.
                    if bytes.first() != Some(&0xf0) {
                        bytes.insert(0, 0xf0);
                    }
                    if bytes.last() != Some(&0xf7) {
                        bytes.push(0xf7);
                    }
                    MidiMsg::Long(bytes)
                } else if len <= 3 {
                    let mut m = [0u8; 3];
                    m[..len].copy_from_slice(&bytes);
                    MidiMsg::Short(m)
                } else {
                    MidiMsg::Long(bytes)
                };
                self.midi_out.push((offset, msg));
                len as f64
            }
            FileOpen => {
                let chosen = match args.first() {
                    Some(Node::Var(slot)) => self.file_sliders.iter().find(|(s, _)| s == slot).map(|(_, files)| {
                        let i = self.vars.at(*slot).round();
                        (i >= 0.0).then(|| files.get(i as usize).cloned()).flatten()
                    }),
                    _ => None,
                };
                let path = match chosen {
                    Some(path) => path,
                    None => {
                        let handle = arg(self, 0);
                        let name = self.string(handle);
                        let name = std::path::Path::new(&name);
                        if name.is_absolute() {
                            Some(name.to_path_buf())
                        } else {
                            self.data_dirs.iter().map(|d| d.join(name)).find(|p| p.is_file())
                        }
                    }
                };
                match path.and_then(|p| std::fs::read(p).ok()) {
                    Some(bytes) => {
                        let file = Some(files::OpenFile::new(bytes));
                        match self.files.iter().position(Option::is_none) {
                            Some(i) => {
                                self.files[i] = file;
                                (i + 1) as f64
                            }
                            None => {
                                self.files.push(file);
                                self.files.len() as f64
                            }
                        }
                    }
                    None => -1.0,
                }
            }
            FileClose => {
                let h = arg(self, 0);
                if let Some(f) = self.file_slot(h) {
                    *f = None;
                }
                0.0
            }
            FileRewind => {
                let h = arg(self, 0);
                if let Some(Some(f)) = self.file_slot(h) {
                    f.rewind();
                }
                0.0
            }
            FileRiff => {
                let h = arg(self, 0);
                let (channels, rate) = match self.file_slot(h) {
                    Some(Some(f)) => f.riff().unwrap_or((0, 0)),
                    _ => (0, 0),
                };
                self.store(&args[1], channels as f64);
                self.store(&args[2], rate as f64);
                channels as f64
            }
            FileText => {
                let h = arg(self, 0);
                match self.file_slot(h) {
                    Some(Some(f)) => f.riff().is_none() as u8 as f64,
                    _ => 0.0,
                }
            }
            FileAvail => {
                let h = arg(self, 0);
                if let Some(serial) = self.serial.as_ref().filter(|_| h.round() == 0.0) {
                    return if serial.writing { -1.0 } else { (serial.values.len() - serial.pos) as f64 };
                }
                match self.file_slot(h) {
                    Some(Some(f)) => f.avail(),
                    _ => 0.0,
                }
            }
            FileVar => {
                let h = arg(self, 0);
                if h.round() == 0.0 && self.serial.is_some() {
                    let v = arg(self, 1);
                    let serial = self.serial.as_mut().unwrap();
                    if serial.writing {
                        serial.values.push(v);
                    } else if let Some(v) = serial.next() {
                        self.store(&args[1], v);
                    }
                    return 1.0;
                }
                let Some(Some(f)) = self.file_slot(h) else { return 0.0 };
                match f.next() {
                    Some(v) => {
                        self.store(&args[1], v);
                        1.0
                    }
                    None => 0.0,
                }
            }
            FileMem => {
                let (h, at, len) = (arg(self, 0), arg(self, 1), arg(self, 2));
                let len = (len.max(0.0) as usize).min(MEM_SIZE);
                if h.round() == 0.0 && self.serial.as_ref().is_some_and(|s| s.writing) {
                    let values: Vec<f64> = (0..len).map(|k| self.read(at + k as f64)).collect();
                    self.serial.as_mut().unwrap().values.extend(values);
                    return len as f64;
                }
                if h.round() == 0.0
                    && let Some(serial) = self.serial.as_mut()
                {
                    let values: Vec<f64> = (0..len).map_while(|_| serial.next()).collect();
                    for (k, &v) in values.iter().enumerate() {
                        self.write(at + k as f64, v);
                    }
                    return values.len() as f64;
                }
                let mut read = 0;
                for k in 0..len {
                    let Some(Some(f)) = self.file_slot(h) else { break };
                    let Some(v) = f.next() else { break };
                    self.write(at + k as f64, v);
                    read += 1;
                }
                read as f64
            }
            FileString => {
                let (h, target) = (arg(self, 0), arg(self, 1));
                let line = match self.file_slot(h) {
                    Some(Some(f)) => f.line(),
                    _ => None,
                };
                match line {
                    Some(line) => {
                        let n = line.len();
                        self.set_string(target, line);
                        n as f64
                    }
                    None => 0.0,
                }
            }
            AtomicGet => arg(self, 0),
            AtomicSet | AtomicAdd => {
                let v = arg(self, 1);
                let new = if matches!(b, AtomicAdd) { arg(self, 0) + v } else { v };
                self.store(place(&args[0]), new)
            }
            AtomicExch => {
                let (a, v) = (arg(self, 0), arg(self, 1));
                self.store(place(&args[1]), a);
                self.store(place(&args[0]), v)
            }
            AtomicSetIfEqual => {
                let (old, value, comp) = (arg(self, 0), arg(self, 1), arg(self, 2));
                if (old - comp).abs() < CLOSE {
                    self.store(place(&args[0]), value);
                }
                old
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
            Fft | Ifft | FftPermute | FftIpermute | FftReal | IfftReal | Mdct | Imdct => {
                let (at, size) = (arg(self, 0), arg(self, 1));
                let n = spectral::pow2_floor(size);
                // Valid sizes as REAPER has them: others leave the buffer alone, real FFTs
                // and MDCTs beyond the largest work at the largest.
                let (len, valid) = match b {
                    FftReal | IfftReal => (n.min(32768), n >= 16),
                    Mdct | Imdct => (n.min(4096), n >= 32),
                    _ => (2 * n, (16..=16384).contains(&n)),
                };
                if valid {
                    let mut data: Vec<f64> = (0..len).map(|k| self.read(at + k as f64)).collect();
                    let s = &mut self.spectral;
                    match b {
                        Fft => s.fft(&mut data, false),
                        Ifft => s.fft(&mut data, true),
                        FftPermute => s.permute(&mut data, false),
                        FftIpermute => s.permute(&mut data, true),
                        FftReal => s.fft_real(&mut data, false),
                        IfftReal => s.fft_real(&mut data, true),
                        Mdct => s.mdct(&mut data, false),
                        _ => s.mdct(&mut data, true),
                    }
                    let written = if matches!(b, Mdct) { len / 2 } else { len };
                    for (k, v) in data.into_iter().take(written).enumerate() {
                        self.write(at + k as f64, v);
                    }
                }
                at
            }
            ConvolveC => {
                let (dst, src, size) = (arg(self, 0), arg(self, 1), arg(self, 2));
                for k in 0..(size.max(0.0) as usize).min(MEM_SIZE / 2) {
                    let i = (2 * k) as f64;
                    let x = (self.read(dst + i), self.read(dst + i + 1.0));
                    let y = (self.read(src + i), self.read(src + i + 1.0));
                    self.write(dst + i, x.0 * y.0 - x.1 * y.1);
                    self.write(dst + i + 1.0, x.0 * y.1 + x.1 * y.0);
                }
                dst
            }
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
            StrcpySubstr => {
                let (dst, src, offset) = (arg(self, 0), arg(self, 1), arg(self, 2));
                let s = self.string(src).into_bytes();
                let from = if offset < 0.0 {
                    s.len().saturating_sub(-offset as usize)
                } else {
                    (offset as usize).min(s.len())
                };
                let len = if args.len() > 3 { arg(self, 3) } else { f64::MAX };
                let to = if len < 0.0 {
                    s.len().saturating_sub(-len as usize).max(from)
                } else {
                    from.saturating_add(len.min(1e15) as usize).min(s.len())
                };
                self.set_string(dst, String::from_utf8_lossy(&s[from..to]).into_owned());
                dst
            }
            StrSetchar => {
                let (h, offset, value) = (arg(self, 0), arg(self, 1), arg(self, 2));
                let kind = if args.len() > 3 { arg(self, 3) as u32 } else { 0 };
                let mut s = self.string(h).into_bytes();
                let at = if offset < 0.0 { s.len() as f64 + offset } else { offset };
                // The type (`'c'`, `'s'`, `'i'`, `'f'`, `'d'`, uppercase big-endian, a `u`
                // for unsigned) is packed into one number, as `'iu'` is.
                let letters: Vec<u8> =
                    kind.to_be_bytes().into_iter().filter(|&c| c != 0 && c != b'u' && c != b'U').collect();
                let bytes = match letters.first().copied().unwrap_or(b'c') {
                    b's' => (value as i64 as u16).to_le_bytes().to_vec(),
                    b'S' => (value as i64 as u16).to_be_bytes().to_vec(),
                    b'i' => (value as i64 as u32).to_le_bytes().to_vec(),
                    b'I' => (value as i64 as u32).to_be_bytes().to_vec(),
                    b'f' => (value as f32).to_le_bytes().to_vec(),
                    b'F' => (value as f32).to_be_bytes().to_vec(),
                    b'd' => value.to_le_bytes().to_vec(),
                    b'D' => value.to_be_bytes().to_vec(),
                    _ => vec![(value as i64 & 0xff) as u8],
                };
                if at >= 0.0 && (at as usize) <= s.len() {
                    let at = at as usize;
                    if s.len() < at + bytes.len() {
                        s.resize(at + bytes.len(), 0);
                    }
                    s[at..at + bytes.len()].copy_from_slice(&bytes);
                    self.set_string(h, String::from_utf8_lossy(&s).into_owned());
                }
                h
            }
            StrcpyFromslider => {
                let dst = arg(self, 0);
                let name = match args.get(1) {
                    Some(Node::Var(slot)) => {
                        self.file_sliders.iter().find(|(s, _)| s == slot).and_then(|(_, files)| {
                            let i = self.vars.at(*slot).round();
                            files.get(i.max(0.0) as usize)?.file_name().map(|n| n.to_string_lossy().into_owned())
                        })
                    }
                    _ => None,
                };
                self.set_string(dst, name.unwrap_or_default());
                dst
            }
            Match(exact) => {
                let (p, t) = (arg(self, 0), arg(self, 1));
                let Some(fields) = matching::matches(&self.string(p), &self.string(t), !exact) else { return 0.0 };
                for (target, f) in args.iter().skip(2).zip(fields) {
                    match f {
                        matching::Field::Num(v) => {
                            self.store(target, v);
                        }
                        matching::Field::Text(s) => {
                            let h = self.eval(target);
                            self.set_string(h, s);
                        }
                    }
                }
                1.0
            }
            HostNumchan => self.vars.get("num_ch").filter(|&n| n > 0.0).unwrap_or(2.0),
            PinMapping => {
                let (pin, start) = (arg(self, 1), arg(self, 2));
                arg(self, 0);
                arg(self, 3);
                let k = pin - start;
                if (0.0..32.0).contains(&k) { (1u64 << k as u32) as f64 } else { 0.0 }
            }
            Gfx(call) => self.gfx_call(call, args),
            Host(index) => self.host_call(index, args),
        }
    }
}

fn truth(x: f64) -> bool {
    x.abs() >= CLOSE
}

/// A truncating conversion as x86-64's `cvttsd2si` does it: NaN and values out of range
/// give the most negative integer.
fn int(x: f64) -> i64 {
    let limit = 2f64.powi(63);
    if x.is_nan() || !(-limit..limit).contains(&x) { i64::MIN } else { x as i64 }
}

/// The same conversion to 32 bits, which REAPER's compile-time shifts use.
fn int32(x: f64) -> i32 {
    if x.is_nan() || !(-2_147_483_648.0..2_147_483_648.0).contains(&x) { i32::MIN } else { x as i32 }
}

/// REAPER runs JSFX with flush-to-zero: arithmetic results too small to be normal become 0.
fn flush(x: f64) -> f64 {
    if x.is_subnormal() { 0.0f64.copysign(x) } else { x }
}

/// What a checked assignment stores: values with the smallest two exponents (zero, subnormal
/// and the lowest normal binade), infinities and NaN become 0.
fn assign_filter(v: f64) -> f64 {
    let e = (v.to_bits() >> 52) & 0x7ff;
    if e <= 1 || e == 0x7ff { 0.0 } else { v }
}

/// EEL2's `invsqrt`: the bit-trick estimate with one Newton step, through a 32-bit float.
fn invsqrt(x: f64) -> f64 {
    let bits = (x as f32).to_bits() as i32 as i64;
    let y = f64::from(f32::from_bits((0x5f37_59df - (bits >> 1)) as u32));
    (x * -0.5 * y * y + 1.5) * y
}

/// The math functions, shared by evaluation and compile-time folding.
fn math(b: Builtin, x: f64, y: f64) -> f64 {
    use Builtin::*;
    match b {
        // The library functions run with flush-to-zero too: a result too small to be normal
        // is 0 (measured with `exp(-740)` and `pow(2, -1070)`).
        Sin => flush(x.sin()),
        Cos => x.cos(),
        Tan => flush(x.tan()),
        Asin => flush(x.asin()),
        Acos => x.acos(),
        Atan => flush(x.atan()),
        Atan2 => flush(x.atan2(y)),
        Sqr => flush(x * x),
        Sqrt => x.abs().sqrt(),
        Pow => flush(x.powf(y)),
        Exp => flush(x.exp()),
        Log => flush(x.ln()),
        Log10 => flush(x.log10()),
        Abs => x.abs(),
        // Unordered comparisons give the first argument.
        Min => {
            if y < x {
                y
            } else {
                x
            }
        }
        Max => {
            if y > x {
                y
            } else {
                x
            }
        }
        // Where REAPER needs the result as a pointer it compares the other way round, so NaN
        // gives the second argument.
        MaxPtr => {
            if x > y {
                x
            } else {
                y
            }
        }
        // Zeros come back as they are; anything else, NaN too, by its sign bit.
        Sign => {
            if x.to_bits() << 1 == 0 {
                x
            } else if x.is_sign_negative() {
                -1.0
            } else {
                1.0
            }
        }
        Floor => x.floor(),
        Ceil => x.ceil(),
        Invsqrt => invsqrt(x),
        _ => 0.0,
    }
}

/// Calls REAPER evaluates at compile time when every argument is a constant: the ones backed
/// by C library functions.
fn folded_math(b: Builtin, args: &[f64]) -> Option<f64> {
    use Builtin::*;
    match (b, args) {
        (Sin | Cos | Tan | Sqrt | Log | Log10 | Asin | Acos | Atan | Exp | Floor | Ceil, [x]) => Some(math(b, *x, 0.0)),
        (Atan2 | Pow, [x, y]) => Some(math(b, *x, *y)),
        _ => None,
    }
}

/// An operator as REAPER's JSFX code runs it.
fn binop(op: Op, a: f64, b: f64) -> f64 {
    use Op::*;
    match op {
        Add | Sub | Mul | Div => flush(arith(op, a, b)),
        // 64-bit shifts of the low 32 bits, read back as a signed 32-bit value.
        Shl => (((int(a) as u64) & 0xffff_ffff) << (int(b) as u64 & 63)) as u32 as i32 as f64,
        Shr => (((int(a) as u64) & 0xffff_ffff) >> (int(b) as u64 & 63)) as u32 as i32 as f64,
        _ => common(op, a, b),
    }
}

/// An operator as REAPER folds it at compile time, without flush-to-zero and with C's
/// 32-bit shifts.
fn fold_value(op: Op, a: f64, b: f64) -> f64 {
    use Op::*;
    match op {
        Add | Sub | Mul | Div => arith(op, a, b),
        Shl => int32(a).wrapping_shl(int32(b) as u32) as f64,
        Shr => int32(a).wrapping_shr(int32(b) as u32) as f64,
        _ => common(op, a, b),
    }
}

fn arith(op: Op, a: f64, b: f64) -> f64 {
    match op {
        Op::Add => a + b,
        Op::Sub => a - b,
        Op::Mul => a * b,
        _ => a / b,
    }
}

/// The operators that run and fold alike. Comparisons follow x86's unordered flags: a NaN
/// operand makes `<`, `<=`, `==` and `===` true and the others false.
fn common(op: Op, a: f64, b: f64) -> f64 {
    use Op::*;
    let unordered = a.is_nan() || b.is_nan();
    let r = match op {
        Mod => {
            let d = int(b.abs()) as u64;
            return if d == 0 { 0.0 } else { ((int(a.abs()) as u64) % d) as f64 };
        }
        Pow => return flush(a.powf(b)),
        Or => return (int(a) | int(b)) as f64,
        And => return (int(a) & int(b)) as f64,
        Xor => return (int(a) ^ int(b)) as f64,
        // Equal infinities differ by NaN, which compares as close.
        Eq => (a - b).abs() < CLOSE || (a - b).is_nan(),
        Ne => (a - b).abs() >= CLOSE,
        ExactEq => a == b || unordered,
        ExactNe => a != b && !unordered,
        Lt => a < b || unordered,
        Gt => a > b,
        Le => a <= b || unordered,
        Ge => a >= b,
        Add | Sub | Mul | Div | Shl | Shr => return binop(op, a, b),
    };
    r as u8 as f64
}

/// Folds an operator over constants and drops the identities REAPER's optimizer removes
/// (`x + 0`, `0 - x` as `-x`, `x * 1`, `x * 0` as 0, ...).
fn fold_bin(op: Op, a: Node, b: Node) -> Node {
    use Op::*;
    match (op, a, b) {
        (op, Node::Num(x), Node::Num(y)) => Node::Num(fold_value(op, x, y)),
        (Add | Sub, a, Node::Num(0.0))
        | (Add, Node::Num(0.0), a)
        | (Mul | Div, a, Node::Num(1.0))
        | (Mul, Node::Num(1.0), a) => a,
        (Sub, Node::Num(0.0), b) => Node::Neg(Box::new(b)),
        (Mul, a, Node::Num(0.0)) | (Mul | Div, Node::Num(0.0), a) => {
            if pure(&a) {
                Node::Num(0.0)
            } else {
                Node::Seq(vec![a, Node::Num(0.0)])
            }
        }
        (op, a, b) => Node::Bin(op, Box::new(a), Box::new(b)),
    }
}

/// Builtins REAPER compiles as machine code taking and giving numbers; the others take their
/// arguments as pointers to values.
fn takes_numbers(b: Builtin) -> bool {
    use Builtin::*;
    matches!(
        b,
        Sin | Cos
            | Tan
            | Asin
            | Acos
            | Atan
            | Atan2
            | Sqr
            | Sqrt
            | Pow
            | Exp
            | Log
            | Log10
            | Abs
            | Min
            | Max
            | MaxPtr
            | Sign
            | Rand
            | Floor
            | Ceil
            | Invsqrt
    )
}

/// Switches `max` to [`Builtin::MaxPtr`] where REAPER needs its result as a pointer (measured
/// with NaN arguments): as an argument of a builtin taking pointers, and as a branch of `?:`
/// whose value is assigned or passed that way, or the last statement of such a branch.
fn mark_pointers(n: &mut Node) {
    fn as_pointer(n: &mut Node) {
        match n {
            Node::Call(b @ Builtin::Max, _) => *b = Builtin::MaxPtr,
            Node::Cond(_, y, n) => {
                as_pointer(y);
                if let Some(n) = n {
                    as_pointer(n);
                }
            }
            Node::Seq(items) => {
                if let Some(last) = items.last_mut() {
                    as_pointer(last);
                }
            }
            _ => {}
        }
    }
    fn assigned(n: &mut Node) {
        match n {
            Node::Cond(..) => as_pointer(n),
            Node::Seq(items) => {
                if let Some(last) = items.last_mut() {
                    assigned(last);
                }
            }
            _ => {}
        }
    }
    match n {
        Node::Num(_) | Node::Var(_) => {}
        Node::Assign(t, v, _) => {
            mark_pointers(t);
            mark_pointers(v);
            assigned(v);
        }
        Node::Mem(a, b)
        | Node::OpAssign(_, a, b)
        | Node::Bin(_, a, b)
        | Node::AndAlso(a, b)
        | Node::OrElse(a, b)
        | Node::Loop(a, b) => {
            mark_pointers(a);
            mark_pointers(b);
        }
        Node::Neg(a) | Node::Not(a) | Node::While(a) => mark_pointers(a),
        Node::Cond(c, y, n) => {
            mark_pointers(c);
            mark_pointers(y);
            if let Some(n) = n {
                mark_pointers(n);
            }
        }
        Node::Seq(items) => items.iter_mut().for_each(mark_pointers),
        Node::Call(b, args) => {
            let pointers = !takes_numbers(*b);
            for a in args {
                mark_pointers(a);
                if pointers {
                    as_pointer(a);
                }
            }
        }
    }
}

fn pure(n: &Node) -> bool {
    match n {
        Node::Num(_) | Node::Var(_) => true,
        Node::Mem(a, b) | Node::Bin(_, a, b) => pure(a) && pure(b),
        Node::Neg(a) | Node::Not(a) => pure(a),
        _ => false,
    }
}

/// Settles a compiled tree as REAPER emits it: constants that are not finite or are
/// subnormal become 0, and each assignment learns whether it needs the check.
fn finish(n: &mut Node) {
    match n {
        Node::Num(v) => {
            if v.is_subnormal() || !v.is_finite() {
                *v = 0.0;
            }
        }
        Node::Var(_) => {}
        Node::Mem(a, b)
        | Node::OpAssign(_, a, b)
        | Node::Bin(_, a, b)
        | Node::AndAlso(a, b)
        | Node::OrElse(a, b)
        | Node::Loop(a, b) => {
            finish(a);
            finish(b);
        }
        Node::Assign(t, v, checked) => {
            // Decided on the constants as folded, before they are emitted.
            *checked = *checked && could_denormal(v);
            finish(t);
            finish(v);
        }
        Node::Neg(a) | Node::Not(a) | Node::While(a) => finish(a),
        Node::Cond(c, y, n) => {
            finish(c);
            finish(y);
            if let Some(n) = n {
                finish(n);
            }
        }
        Node::Seq(items) | Node::Call(_, items) => items.iter_mut().for_each(finish),
    }
}

/// Whether REAPER's compiler checks an assignment of this value: it does unless the value
/// comes from an operation that cannot produce a subnormal (or infinity or NaN) from clean
/// inputs. Some operations always give clean results (`+ c` and `- c` with a constant beyond
/// 1e-50, integer operators, comparisons, `cos`, `floor`, ...), some keep their inputs'
/// cleanliness (`* c` with |c| >= 1, `/ c` or `c /` with |c| <= 1, negation, `sin`, `sqrt`,
/// `abs`, `min`, `max`), and the rest may produce anything.
fn could_denormal(n: &Node) -> bool {
    use Builtin::*;
    match n {
        Node::Bin(op, a, b) => {
            let c = match (&**a, &**b) {
                (_, Node::Num(c)) | (Node::Num(c), _) => Some(c.abs()),
                _ => None,
            };
            match op {
                Op::Add | Op::Sub => !c.is_some_and(|c| c > 1e-50),
                Op::Mul if c.is_some_and(|c| c >= 1.0) => could_denormal(a) || could_denormal(b),
                Op::Div if c.is_some_and(|c| c <= 1.0) => could_denormal(a) || could_denormal(b),
                Op::Mul | Op::Div | Op::Pow => true,
                _ => false,
            }
        }
        Node::Neg(a) => could_denormal(a),
        Node::Cond(_, y, n) => could_denormal(y) || n.as_deref().is_some_and(could_denormal),
        Node::Seq(items) => items.last().is_some_and(could_denormal),
        Node::Call(b, args) => match b {
            Sin | Sqrt | Abs | Min | Max | MaxPtr => args.iter().any(could_denormal),
            Tan | Asin | Acos | Atan | Atan2 | Exp | Log | Log10 | Sqr | Invsqrt | Pow => true,
            _ => false,
        },
        _ => false,
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
        // REAPER's Spectral Hold relies on this: the instance namespace is `gain.parmchg`.
        assert_eq!(var(&m, "gain.parmchg.x"), 3.0);
        assert_eq!(var(&m, "spl0"), 6.0);
    }

    #[test]
    fn atomics_and_buffer_midi() {
        let mut m = run("a = 1; b = 2; atomic_add(a, 5); r = atomic_exch(a = 0, b); \
             atomic_setifequal(b, 9, 0); old = atomic_setifequal(b, 7, 1);");
        assert_eq!((var(&m, "a"), var(&m, "b"), var(&m, "r"), var(&m, "old")), (2.0, 9.0, 2.0, 9.0));
        let code = compile(
            "n = midirecv_buf(at, 100, 3); midisend_buf(at + 1, 100, n); c = midirecv_buf(at2, 200, 1);",
            &mut m.vars,
            &mut HashMap::new(),
        )
        .unwrap();
        m.set_midi([(4.0, MidiMsg::Short([0x90, 60, 100])), (5.0, MidiMsg::Short([0xc0, 3, 0]))].into_iter());
        m.run(&code);
        assert_eq!((var(&m, "n"), var(&m, "at"), var(&m, "c")), (3.0, 4.0, 0.0), "a 2-byte message does not fit 1");
        assert_eq!(m.midi_out, [(5.0, MidiMsg::Short([0x90, 60, 100]))]);
    }

    #[test]
    fn assignments_clear_what_could_be_non_finite() {
        // Measured in REAPER: an assignment drops NaN and infinities unless the value comes
        // from an operation that cannot make them out of clean inputs.
        let m = run("z = 0; a = log(z); b = log(z) + 1; c = z / z; d = (z / z) + 1; e = 1 / z; f = 1 / 0; \
             g = sqrt(z - 4); h = abs(z / z); i = abs(d); j = d * 0.5; k = d * 2; l = 0 - d; \
             p = min(d, 1); q = max(1, d); r = cos(d); s = tan(d); x = d + 0;");
        let v = |n: &str| var(&m, n);
        assert_eq!(
            (v("a"), v("b"), v("c"), v("e"), v("f"), v("g")),
            (0.0, f64::NEG_INFINITY, 0.0, f64::INFINITY, 0.0, 2.0)
        );
        assert!(v("d").is_nan() && v("i").is_nan() && v("k").is_nan() && v("l").is_nan() && v("x").is_nan());
        assert_eq!((v("h"), v("j"), v("s"), v("q")), (0.0, 0.0, 0.0, 1.0));
        assert!(v("p").is_nan() && v("r").is_nan(), "min and max give their first argument for NaN");
    }

    #[test]
    fn nan_comparisons_follow_unordered_flags() {
        let m = run("z = 0; n = z / z + 1; a = n < 1; b = n <= 1; c = n == 1; d = n === 1; e = n > 1; f = n >= 1; \
             g = n != 1; h = n ? 1 : 2; i = n | 0; j = n % 7; k = sign(n);");
        let v: Vec<f64> = ["a", "b", "c", "d", "e", "f", "g", "h"].iter().map(|n| var(&m, n)).collect();
        assert_eq!(v, [1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 2.0]);
        assert_eq!((var(&m, "i"), var(&m, "j"), var(&m, "k")), (i64::MIN as f64, 1.0, -1.0));
    }

    #[test]
    fn max_for_a_pointer_gives_the_second_argument_for_nan() {
        let m = run("z = 0; c = 1; n = z / z + 1; a = max(n, 5); b = c ? max(n, 5) : 0; memset(100, max(n, 5), 1); \
             p = 100[0]; d = c ? (y = 1; max(n, 5)) : 0; e = (c ? max(n, 5) : 0) + 1; f = c ? min(n, 5) : 0;");
        assert!(var(&m, "a").is_nan() && var(&m, "e").is_nan() && var(&m, "f").is_nan(), "numbers give the first");
        assert_eq!([var(&m, "b"), var(&m, "p"), var(&m, "d")], [5.0; 3]);
    }

    #[test]
    fn exponents_do_not_compile_and_library_results_flush() {
        for src in
            ["x = 1e5;", "x = 1E5;", "x = 1e-5;", "x = .5e2;", "x = 1.e3;", "x = 3e2x;", "x = 1d5;", "x = 1.5.5;"]
        {
            assert!(compile(src, &mut Vars::default(), &mut HashMap::new()).is_err(), "{src}");
        }
        let m = run("x = 0x1e5; y = .5; z = 0; m = -740; a = exp(m + z) * 2 ^ 1000; b = pow(2, -1070 + z) * 2 ^ 1000;");
        assert_eq!([var(&m, "x"), var(&m, "y"), var(&m, "a"), var(&m, "b")], [485.0, 0.5, 0.0, 0.0]);
    }

    #[test]
    fn shifts_flush_to_zero_invsqrt_and_loop() {
        let m = run("z = 0; a = (z - 8) >> 1; b = (z + 1) << 40; c = -8 >> 1; d = 1 << 40; \
             t = 1; loop(1022, t *= 0.5); e = t * 0.5 * 4; \
             f = invsqrt(z + 4); g = loop(3, 0);");
        assert_eq!((var(&m, "a"), var(&m, "b"), var(&m, "c"), var(&m, "d")), (2147483644.0, 0.0, -4.0, 256.0));
        assert_eq!(var(&m, "e"), 0.0, "a subnormal product is flushed");
        assert_eq!((var(&m, "f") as f32, var(&m, "g")), (0.49915358, 1.0));
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

    #[test]
    fn gfx_era_syntax() {
        let m = run("x = 1; x > 0 && y = 5; 0 || z = 'ab'; loop(2, w += 1;;); #s = \"a\" \"b\"; n = strlen(#s); \
             strcpy_substr(#t, \"hello\", 1, 3); same = strcmp(#t, \"ell\") == 0; \
             hit = match(\"%d-*\", \"42-x\", v);");
        assert_eq!((var(&m, "y"), var(&m, "z")), (5.0, 97.0 * 256.0 + 98.0), "the rightmost operand is assigned");
        assert_eq!((var(&m, "w"), var(&m, "n"), var(&m, "same")), (2.0, 2.0, 1.0));
        assert_eq!((var(&m, "hit"), var(&m, "v")), (1.0, 42.0));
    }
}
