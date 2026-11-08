//! Host functions: names a program calls that the embedder answers, as ReaScript's API is.
//! Their arguments are passed as expressions, so a host function can write back to the
//! variables and strings it was handed (`GetTrackName(tr, #name)`).

use std::cell::RefCell;
use std::collections::HashMap;

use super::{Code, Function, Machine, Node, Parser, Resolver, Scope, Vars, finish, lex, place};

/// Answers the host functions of a program compiled with `compile_with_host`.
pub trait Host {
    /// Calls the host function at `index` in the names the program was compiled with.
    fn call(&mut self, index: usize, args: &mut HostArgs) -> f64;
}

/// The arguments of a host call, evaluated once, left to right.
pub struct HostArgs<'a> {
    machine: &'a mut Machine,
    nodes: &'a [Node],
    values: Vec<f64>,
}

impl HostArgs<'_> {
    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// The argument's value, 0 when it was not passed.
    pub fn value(&self, i: usize) -> f64 {
        self.values.get(i).copied().unwrap_or(0.0)
    }

    /// The string the argument's value is a handle of.
    pub fn string(&self, i: usize) -> String {
        self.machine.string(self.value(i))
    }

    /// Writes to the variable or memory cell passed as the argument, if it was one.
    pub fn set_value(&mut self, i: usize, v: f64) {
        if let Some(n) = self.nodes.get(i) {
            self.machine.store(place(n), v);
        }
    }

    /// Writes to the string the argument is a handle of, if it is a writable one.
    pub fn set_string(&mut self, i: usize, s: String) {
        if i < self.values.len() {
            self.machine.set_string(self.values[i], s);
        }
    }

    /// The arguments from `i` on formatted as `sprintf` does, argument `i` being the format.
    pub fn format(&mut self, i: usize) -> String {
        let fmt = self.string(i);
        let mut next = i + 1;
        let values = &self.values;
        self.machine.format_with(&fmt, |_| {
            let v = values.get(next).copied().unwrap_or(0.0);
            next += 1;
            v
        })
    }

    /// The machine the call runs on, for host functions that run code of their own.
    pub fn machine(&mut self) -> &mut Machine {
        self.machine
    }
}

thread_local! {
    /// The host of the program running on this thread; `Machine` itself stays `Send`.
    static HOST: RefCell<Option<Box<dyn Host>>> = const { RefCell::new(None) };
}

/// Compiles `src` as `compile` does, resolving calls of `host` names (in any case) that are not
/// builtins or user functions to host calls.
pub fn compile_with_host(
    src: &str,
    vars: &mut Vars,
    functions: &mut HashMap<String, Function>,
    host: &[&str],
) -> Result<Code, String> {
    let mut p = Parser { toks: lex(src)?, at: 0, functions: std::mem::take(functions) };
    let ast = p.sequence(None);
    *functions = p.functions;
    let ast = ast?;
    let mut r = Resolver { vars, functions, host };
    let mut node = r.resolve(&Scope::default(), &ast)?;
    finish(&mut node);
    Ok(Code(node))
}

impl Machine {
    /// Runs `code` with `host` answering its host calls.
    pub fn run_hosted(&mut self, code: &Code, host: Box<dyn Host>) -> f64 {
        let outer = HOST.with(|h| h.replace(Some(host)));
        let v = self.run(code);
        HOST.with(|h| *h.borrow_mut() = outer);
        v
    }

    pub(super) fn host_call(&mut self, index: usize, args: &[Node]) -> f64 {
        let values = args.iter().map(|a| self.eval(a)).collect();
        let Some(mut host) = HOST.with(|h| h.borrow_mut().take()) else { return 0.0 };
        let v = host.call(index, &mut HostArgs { machine: self, nodes: args, values });
        HOST.with(|h| *h.borrow_mut() = Some(host));
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;

    impl Host for Echo {
        fn call(&mut self, index: usize, args: &mut HostArgs) -> f64 {
            match index {
                0 => {
                    let s = format!("<{}>", args.string(0));
                    args.set_string(1, s);
                    args.set_value(2, 7.0);
                    1.0
                }
                _ => args.value(0) * 2.0,
            }
        }
    }

    #[test]
    fn host_functions_return_and_write_back() {
        let mut vars = Vars::default();
        let mut fns = HashMap::new();
        let src = "r = Wrap(\"a\", #out, n); d = double(21); function f() ( double(1) ); e = f();";
        let code = compile_with_host(src, &mut vars, &mut fns, &["Wrap", "double"]).unwrap();
        let mut m = Machine::new(vars);
        m.run_hosted(&code, Box::new(Echo));
        let get = |n: &str| m.vars.get(n).unwrap();
        assert_eq!((get("r"), get("n"), get("d"), get("e")), (1.0, 7.0, 42.0, 2.0));
        let out = m.vars.named["#out"];
        assert_eq!(m.string(out as f64), "<a>");
        assert!(compile_with_host("nope()", &mut Vars::default(), &mut HashMap::new(), &["Wrap"]).is_err());
    }
}
