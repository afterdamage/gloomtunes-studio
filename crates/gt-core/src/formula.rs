//! A small expression language for formula modulators (Prompt 34, D151).
//!
//! A formula is compiled once on the UI thread into a fixed-size [`Program`] (no heap, `Copy`),
//! which the audio thread evaluates every control quantum with a fixed stack. The result is a
//! modulator output, clamped to -1..1.
//!
//! ```text
//! 0.5 * sin(2 * pi * t) * m3        a sine at 1 Hz, scaled by modulator 3
//! tri(beat / 4) * wheel             a triangle over a bar of 4/4, depth on the mod wheel
//! if(m1 > 0, m1, -m2)               pick between two modulators
//! ```
//!
//! Numbers, `+ - * / % ^`, unary minus, comparisons `< > <= >=` (1 or 0), parentheses, and:
//!
//! * variables: `t` (seconds the modulator has run), `beat` (song position in quarter notes,
//!   running on at the tempo while stopped), `bpm`, `wheel` and `touch` (mod wheel and
//!   aftertouch, 0 to 1), `pi`, and `m1`, `m2`, … the output of the modulator with that number
//!   (the previous control quantum's, so modulators can read each other in any order);
//! * functions of radians `sin cos`, of cycles `tri saw sqr` (-1 to 1), `abs floor frac sqrt
//!   exp log sign`, `hash` (a repeatable random value in -1..1 for each whole number), `min
//!   max pow step` of two values, `clamp lerp if` of three.

use crate::modulation::ModulatorId;

/// Longest program, in operations.
pub const MAX_OPS: usize = 96;
/// Most modulators one formula can read.
pub const MAX_INPUTS: usize = 8;
/// Deepest evaluation stack.
const MAX_STACK: usize = 16;

/// What a formula can read besides other modulators.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct FormulaVars {
    /// Seconds the modulator has run.
    pub t: f32,
    /// Song position in quarter notes.
    pub beat: f32,
    /// Tempo.
    pub bpm: f32,
    /// Mod wheel, 0 to 1.
    pub wheel: f32,
    /// Aftertouch, 0 to 1.
    pub touch: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Var {
    T,
    Beat,
    Bpm,
    Wheel,
    Touch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum F1 {
    Neg,
    Sin,
    Cos,
    Tri,
    Saw,
    Sqr,
    Abs,
    Floor,
    Frac,
    Sqrt,
    Exp,
    Log,
    Sign,
    Hash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum F2 {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
    Lt,
    Gt,
    Le,
    Ge,
    Min,
    Max,
    Step,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum F3 {
    Clamp,
    Lerp,
    If,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Op {
    Const(f32),
    Var(Var),
    /// Input `k` of [`Program::inputs`].
    Input(u8),
    Call1(F1),
    Call2(F2),
    Call3(F3),
}

/// A compiled formula.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Program {
    ops: [Op; MAX_OPS],
    len: u8,
    inputs: [ModulatorId; MAX_INPUTS],
    n_inputs: u8,
}

impl Default for Program {
    /// The formula `0`.
    fn default() -> Self {
        Self {
            ops: [Op::Const(0.0); MAX_OPS],
            len: 1,
            inputs: [ModulatorId(0); MAX_INPUTS],
            n_inputs: 0,
        }
    }
}

/// Why a formula does not compile, with the character position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormulaError {
    /// Byte offset in the text.
    pub at: usize,
    /// What is wrong.
    pub message: String,
}

impl std::fmt::Display for FormulaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (at {})", self.message, self.at + 1)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Tok {
    Num(f32),
    Ident(usize, usize),
    Sym(&'static str),
    End,
}

struct Parser<'a> {
    src: &'a str,
    pos: usize,
    tok: Tok,
    tok_at: usize,
    prog: Program,
    depth: usize,
    max_depth: usize,
}

const SYMS: [&str; 13] = [
    "<=", ">=", "<", ">", "+", "-", "*", "/", "%", "^", "(", ")", ",",
];

impl<'a> Parser<'a> {
    fn err<T>(&self, message: impl Into<String>) -> Result<T, FormulaError> {
        Err(FormulaError {
            at: self.tok_at,
            message: message.into(),
        })
    }

    fn next(&mut self) -> Result<(), FormulaError> {
        let b = self.src.as_bytes();
        while self.pos < b.len() && b[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
        self.tok_at = self.pos;
        if self.pos >= b.len() {
            self.tok = Tok::End;
            return Ok(());
        }
        let c = b[self.pos];
        if c.is_ascii_digit() || c == b'.' {
            let start = self.pos;
            while self.pos < b.len() && (b[self.pos].is_ascii_digit() || b[self.pos] == b'.') {
                self.pos += 1;
            }
            // An exponent: 1e3, 2.5e-2.
            if self.pos < b.len() && (b[self.pos] == b'e' || b[self.pos] == b'E') {
                let save = self.pos;
                self.pos += 1;
                if self.pos < b.len() && (b[self.pos] == b'+' || b[self.pos] == b'-') {
                    self.pos += 1;
                }
                if self.pos < b.len() && b[self.pos].is_ascii_digit() {
                    while self.pos < b.len() && b[self.pos].is_ascii_digit() {
                        self.pos += 1;
                    }
                } else {
                    self.pos = save;
                }
            }
            return match self.src[start..self.pos].parse::<f32>() {
                Ok(v) => {
                    self.tok = Tok::Num(v);
                    Ok(())
                }
                Err(_) => self.err("bad number"),
            };
        }
        if c.is_ascii_alphabetic() || c == b'_' {
            let start = self.pos;
            while self.pos < b.len() && (b[self.pos].is_ascii_alphanumeric() || b[self.pos] == b'_')
            {
                self.pos += 1;
            }
            self.tok = Tok::Ident(start, self.pos);
            return Ok(());
        }
        for s in SYMS {
            if self.src[self.pos..].starts_with(s) {
                self.pos += s.len();
                self.tok = Tok::Sym(s);
                return Ok(());
            }
        }
        self.err(format!(
            "unexpected '{}'",
            self.src[self.pos..].chars().next().unwrap_or('?')
        ))
    }

    fn emit(&mut self, op: Op, pushes: isize) -> Result<(), FormulaError> {
        let len = usize::from(self.prog.len);
        if len >= MAX_OPS {
            return self.err("formula too long");
        }
        self.prog.ops[len] = op;
        self.prog.len += 1;
        self.depth = (self.depth as isize + pushes) as usize;
        self.max_depth = self.max_depth.max(self.depth);
        if self.max_depth > MAX_STACK {
            return self.err("formula nested too deeply");
        }
        Ok(())
    }

    fn eat(&mut self, s: &str) -> Result<bool, FormulaError> {
        if matches!(self.tok, Tok::Sym(x) if x == s) {
            self.next()?;
            return Ok(true);
        }
        Ok(false)
    }

    fn expr(&mut self) -> Result<(), FormulaError> {
        self.sum()?;
        for (s, f) in [("<=", F2::Le), (">=", F2::Ge), ("<", F2::Lt), (">", F2::Gt)] {
            if self.eat(s)? {
                self.sum()?;
                return self.emit(Op::Call2(f), -1);
            }
        }
        Ok(())
    }

    fn sum(&mut self) -> Result<(), FormulaError> {
        self.product()?;
        loop {
            let f = if self.eat("+")? {
                F2::Add
            } else if self.eat("-")? {
                F2::Sub
            } else {
                return Ok(());
            };
            self.product()?;
            self.emit(Op::Call2(f), -1)?;
        }
    }

    fn product(&mut self) -> Result<(), FormulaError> {
        self.unary()?;
        loop {
            let f = if self.eat("*")? {
                F2::Mul
            } else if self.eat("/")? {
                F2::Div
            } else if self.eat("%")? {
                F2::Rem
            } else {
                return Ok(());
            };
            self.unary()?;
            self.emit(Op::Call2(f), -1)?;
        }
    }

    fn unary(&mut self) -> Result<(), FormulaError> {
        if self.eat("-")? {
            self.unary()?;
            return self.emit(Op::Call1(F1::Neg), 0);
        }
        if self.eat("+")? {
            return self.unary();
        }
        self.atom()?;
        if self.eat("^")? {
            // Right-associative, binds tighter than unary minus on its left: -2^2 = -4.
            self.unary()?;
            self.emit(Op::Call2(F2::Pow), -1)?;
        }
        Ok(())
    }

    fn atom(&mut self) -> Result<(), FormulaError> {
        match self.tok {
            Tok::Num(v) => {
                self.next()?;
                self.emit(Op::Const(v), 1)
            }
            Tok::Sym("(") => {
                self.next()?;
                self.expr()?;
                if !self.eat(")")? {
                    return self.err("missing ')'");
                }
                Ok(())
            }
            Tok::Ident(a, b) => {
                let name = &self.src[a..b];
                self.next()?;
                if self.tok == Tok::Sym("(") {
                    self.next()?;
                    return self.call(name, a);
                }
                self.variable(name, a)
            }
            Tok::End => self.err("unexpected end"),
            Tok::Sym(s) => self.err(format!("unexpected '{s}'")),
        }
    }

    fn variable(&mut self, name: &str, at: usize) -> Result<(), FormulaError> {
        let var = match name {
            "t" => Var::T,
            "beat" => Var::Beat,
            "bpm" => Var::Bpm,
            "wheel" => Var::Wheel,
            "touch" => Var::Touch,
            "pi" => return self.emit(Op::Const(core::f32::consts::PI), 1),
            _ => {
                let id = name
                    .strip_prefix('m')
                    .and_then(|n| n.parse::<u32>().ok())
                    .filter(|_| name.len() > 1);
                let Some(id) = id else {
                    self.tok_at = at;
                    return self.err(format!("unknown name '{name}'"));
                };
                let id = ModulatorId(id);
                let p = &mut self.prog;
                let n = usize::from(p.n_inputs);
                let k = match p.inputs[..n].iter().position(|&x| x == id) {
                    Some(k) => k,
                    None if n < MAX_INPUTS => {
                        p.inputs[n] = id;
                        p.n_inputs += 1;
                        n
                    }
                    None => {
                        self.tok_at = at;
                        return self.err(format!("at most {MAX_INPUTS} modulators"));
                    }
                };
                return self.emit(Op::Input(k as u8), 1);
            }
        };
        self.emit(Op::Var(var), 1)
    }

    fn call(&mut self, name: &str, at: usize) -> Result<(), FormulaError> {
        enum Kind {
            One(F1),
            Two(F2),
            Three(F3),
        }
        let kind = match name {
            "sin" => Kind::One(F1::Sin),
            "cos" => Kind::One(F1::Cos),
            "tri" => Kind::One(F1::Tri),
            "saw" => Kind::One(F1::Saw),
            "sqr" => Kind::One(F1::Sqr),
            "abs" => Kind::One(F1::Abs),
            "floor" => Kind::One(F1::Floor),
            "frac" => Kind::One(F1::Frac),
            "sqrt" => Kind::One(F1::Sqrt),
            "exp" => Kind::One(F1::Exp),
            "log" => Kind::One(F1::Log),
            "sign" => Kind::One(F1::Sign),
            "hash" => Kind::One(F1::Hash),
            "min" => Kind::Two(F2::Min),
            "max" => Kind::Two(F2::Max),
            "pow" => Kind::Two(F2::Pow),
            "step" => Kind::Two(F2::Step),
            "clamp" => Kind::Three(F3::Clamp),
            "lerp" => Kind::Three(F3::Lerp),
            "if" => Kind::Three(F3::If),
            _ => {
                self.tok_at = at;
                return self.err(format!("unknown function '{name}'"));
            }
        };
        let want = match kind {
            Kind::One(_) => 1,
            Kind::Two(_) => 2,
            Kind::Three(_) => 3,
        };
        let mut got = 0;
        if !self.eat(")")? {
            loop {
                self.expr()?;
                got += 1;
                if self.eat(")")? {
                    break;
                }
                if !self.eat(",")? {
                    return self.err("expected ',' or ')'");
                }
            }
        }
        if got != want {
            self.tok_at = at;
            return self.err(format!("{name} takes {want} value(s), got {got}"));
        }
        match kind {
            Kind::One(f) => self.emit(Op::Call1(f), 0),
            Kind::Two(f) => self.emit(Op::Call2(f), -1),
            Kind::Three(f) => self.emit(Op::Call3(f), -2),
        }
    }
}

#[inline]
fn fract(x: f32) -> f32 {
    x - x.floor()
}

/// A repeatable value in -1..1 for each whole number (an integer hash).
fn hash(x: f32) -> f32 {
    let mut h = (x.floor() as i64 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    h ^= h >> 29;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 32;
    (h >> 40) as f32 / (1u64 << 23) as f32 - 1.0
}

impl Program {
    /// Compiles `text`.
    pub fn compile(text: &str) -> Result<Self, FormulaError> {
        let mut p = Parser {
            src: text,
            pos: 0,
            tok: Tok::End,
            tok_at: 0,
            prog: Program {
                len: 0,
                ..Program::default()
            },
            depth: 0,
            max_depth: 0,
        };
        p.next()?;
        if p.tok == Tok::End {
            return p.err("empty formula");
        }
        p.expr()?;
        if p.tok != Tok::End {
            return p.err("unexpected text after the formula");
        }
        Ok(p.prog)
    }

    /// The modulators the formula reads, in input order.
    pub fn inputs(&self) -> &[ModulatorId] {
        &self.inputs[..usize::from(self.n_inputs)]
    }

    /// Evaluates with `inputs[k]` the value of [`Self::inputs`]`[k]`. Real-time safe. The
    /// result is clamped to -1..1; a value that is not a number gives 0.
    pub fn eval(&self, v: &FormulaVars, inputs: &[f32; MAX_INPUTS]) -> f32 {
        let mut st = [0.0_f32; MAX_STACK];
        let mut sp = 0usize;
        for op in &self.ops[..usize::from(self.len)] {
            match *op {
                Op::Const(c) => {
                    st[sp] = c;
                    sp += 1;
                }
                Op::Var(var) => {
                    st[sp] = match var {
                        Var::T => v.t,
                        Var::Beat => v.beat,
                        Var::Bpm => v.bpm,
                        Var::Wheel => v.wheel,
                        Var::Touch => v.touch,
                    };
                    sp += 1;
                }
                Op::Input(k) => {
                    st[sp] = inputs[usize::from(k)];
                    sp += 1;
                }
                Op::Call1(f) => {
                    let x = st[sp - 1];
                    st[sp - 1] = match f {
                        F1::Neg => -x,
                        F1::Sin => x.sin(),
                        F1::Cos => x.cos(),
                        F1::Tri => 1.0 - 4.0 * (fract(x + 0.25) - 0.5).abs(),
                        F1::Saw => 2.0 * fract(x) - 1.0,
                        F1::Sqr => {
                            if fract(x) < 0.5 {
                                1.0
                            } else {
                                -1.0
                            }
                        }
                        F1::Abs => x.abs(),
                        F1::Floor => x.floor(),
                        F1::Frac => fract(x),
                        F1::Sqrt => x.max(0.0).sqrt(),
                        F1::Exp => x.min(80.0).exp(),
                        F1::Log => x.max(1e-30).ln(),
                        F1::Sign => {
                            if x > 0.0 {
                                1.0
                            } else if x < 0.0 {
                                -1.0
                            } else {
                                0.0
                            }
                        }
                        F1::Hash => hash(x),
                    };
                }
                Op::Call2(f) => {
                    let (a, b) = (st[sp - 2], st[sp - 1]);
                    sp -= 1;
                    let bool = |c: bool| if c { 1.0 } else { 0.0 };
                    st[sp - 1] = match f {
                        F2::Add => a + b,
                        F2::Sub => a - b,
                        F2::Mul => a * b,
                        F2::Div => {
                            if b == 0.0 {
                                0.0
                            } else {
                                a / b
                            }
                        }
                        F2::Rem => {
                            if b == 0.0 {
                                0.0
                            } else {
                                a.rem_euclid(b)
                            }
                        }
                        F2::Pow => a.powf(b),
                        F2::Lt => bool(a < b),
                        F2::Gt => bool(a > b),
                        F2::Le => bool(a <= b),
                        F2::Ge => bool(a >= b),
                        F2::Min => a.min(b),
                        F2::Max => a.max(b),
                        F2::Step => bool(b >= a),
                    };
                }
                Op::Call3(f) => {
                    let (a, b, c) = (st[sp - 3], st[sp - 2], st[sp - 1]);
                    sp -= 2;
                    st[sp - 1] = match f {
                        F3::Clamp => a.max(b).min(c),
                        F3::Lerp => a + (b - a) * c,
                        F3::If => {
                            if a > 0.0 {
                                b
                            } else {
                                c
                            }
                        }
                    };
                }
            }
        }
        let r = if sp > 0 { st[sp - 1] } else { 0.0 };
        if r.is_finite() {
            r.clamp(-1.0, 1.0)
        } else {
            0.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(s: &str) -> f32 {
        Program::compile(s)
            .unwrap()
            .eval(&FormulaVars::default(), &[0.0; MAX_INPUTS])
    }

    #[test]
    fn arithmetic_and_precedence() {
        assert_eq!(eval("0.1 + 0.2 * 2"), 0.5);
        assert_eq!(eval("(0.1 + 0.2) * 2"), 0.6_f32.min(1.0));
        assert_eq!(eval("-2^2 / 8"), -0.5);
        assert_eq!(eval("2^-1"), 0.5);
        assert_eq!(eval("7 % 4 / 4"), 0.75);
        assert_eq!(eval("1 / 0"), 0.0);
        assert_eq!(eval("5"), 1.0, "clamped");
        assert_eq!(eval("2.5e-1"), 0.25);
        assert_eq!(eval("0.3 < 0.4"), 1.0);
        assert_eq!(eval("if(0.3 >= 0.4, 0.2, -0.2)"), -0.2);
        assert_eq!(eval("clamp(3, -0.5, 0.5)"), 0.5);
        assert_eq!(eval("lerp(-1, 1, 0.75)"), 0.5);
        assert!(eval("log(-1)").is_finite());
    }

    #[test]
    fn shapes() {
        assert!((eval("tri(0)")).abs() < 1e-6);
        assert!((eval("tri(0.25)") - 1.0).abs() < 1e-6);
        assert!((eval("tri(0.75)") + 1.0).abs() < 1e-6);
        assert_eq!(eval("saw(0.5)"), 0.0);
        assert_eq!(eval("sqr(0.2)"), 1.0);
        assert_eq!(eval("sqr(0.7)"), -1.0);
        assert!((eval("sin(pi / 2)") - 1.0).abs() < 1e-6);
        let (a, b) = (eval("hash(3.2)"), eval("hash(3.9)"));
        assert_eq!(a, b, "same whole number");
        assert_ne!(eval("hash(3)"), eval("hash(4)"));
        for k in 0..200 {
            let h = hash(k as f32);
            assert!((-1.0..1.0).contains(&h));
        }
    }

    #[test]
    fn variables_and_inputs() {
        let p = Program::compile("m7 * wheel + m2 - m7 * 0").unwrap();
        assert_eq!(p.inputs(), &[ModulatorId(7), ModulatorId(2)]);
        let vars = FormulaVars {
            wheel: 0.5,
            ..FormulaVars::default()
        };
        let mut ins = [0.0; MAX_INPUTS];
        ins[0] = 0.8;
        ins[1] = 0.1;
        assert!((p.eval(&vars, &ins) - 0.5).abs() < 1e-6);
        let p = Program::compile("saw(beat / 4) * (bpm > 100)").unwrap();
        let v = FormulaVars {
            beat: 3.0,
            bpm: 120.0,
            ..FormulaVars::default()
        };
        assert_eq!(p.eval(&v, &ins), 0.5);
    }

    #[test]
    fn errors_say_where() {
        let e = Program::compile("1 + foo").unwrap_err();
        assert_eq!(e.at, 4);
        assert!(e.message.contains("foo"));
        assert!(Program::compile("").is_err());
        assert!(Program::compile("(1 + 2").is_err());
        assert!(Program::compile("min(1)")
            .unwrap_err()
            .message
            .contains("2"));
        assert!(Program::compile("1 2").is_err());
        assert!(Program::compile("1 $ 2").is_err());
        assert!(Program::compile("m").is_err());
        let deep = "(".repeat(40) + "1" + &")".repeat(40);
        assert!(
            Program::compile(&deep).is_ok(),
            "parentheses alone do not deepen"
        );
        let wide = (0..20).map(|_| "(1+").collect::<String>() + "1" + &")".repeat(20);
        assert!(Program::compile(&wide)
            .unwrap_err()
            .message
            .contains("deep"));
        let long = vec!["1"; 60].join("+");
        assert!(Program::compile(&long)
            .unwrap_err()
            .message
            .contains("long"));
        let many = (1..=9)
            .map(|i| format!("m{i}"))
            .collect::<Vec<_>>()
            .join("+");
        assert!(Program::compile(&many).is_err());
    }
}
