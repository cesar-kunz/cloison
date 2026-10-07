//! Bytecode format: an accumulator + register machine (in the spirit of V8's
//! Ignition). Instructions are variable length: a one-byte opcode followed by
//! its operands, little-endian, unaligned.

/// The kinds of instruction operands, in encoding order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operand {
    /// A register index (1 byte).
    Reg,
    /// A signed 32-bit immediate (4 bytes).
    Imm,
    /// A jump offset relative to the start of the instruction (4 bytes).
    Off,
    /// An inline-cache slot of the function (2 bytes). Each slot also
    /// records the property name it is for.
    Ic,
    /// An index into the function's table of nested functions (2 bytes).
    Fn,
    /// An inline-cache slot of the function, not written in the text
    /// format, which gives each such instruction its own (2 bytes).
    AutoIc,
    /// An argument count (1 byte).
    Count,
    /// An index into the function's constants (2 bytes): a string, or a
    /// number that isn't an int32.
    Const,
}

impl Operand {
    pub const fn width(self) -> usize {
        match self {
            Operand::Reg | Operand::Count => 1,
            Operand::Ic | Operand::Fn | Operand::AutoIc | Operand::Const => 2,
            Operand::Imm | Operand::Off => 4,
        }
    }
}

macro_rules! define_ops {
    ($( $(#[$m:meta])* $name:ident ( $($k:ident),* ) ),* $(,)?) => {
        #[repr(u8)]
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Op {
            $( $(#[$m])* $name, )*
        }

        impl Op {
            pub const ALL: [Op; [$(Op::$name),*].len()] = [$(Op::$name),*];
            pub const COUNT: usize = Op::ALL.len();

            /// The operand kinds, in encoding order.
            pub const fn operands(self) -> &'static [Operand] {
                match self {
                    $( Op::$name => &[$(Operand::$k),*], )*
                }
            }

            /// The mnemonic, as the text assembler spells it.
            pub const fn name(self) -> &'static str {
                match self {
                    $( Op::$name => stringify!($name), )*
                }
            }
        }
    };
}

// Binary operators follow Ignition: `Op r` computes `r OP acc` (the left
// operand is the register), while the `…Smi` forms compute `acc OP imm`.
// So `x + y` is "y into acc, `Add r_x`", and `n += e` is "e into acc,
// `Add r_n`", with no temporary. The fused compare-and-jumps are the
// exception: `JumpIfLess r` tests `acc < r`.
define_ops! {
    /// No operation. Its handler is pure dispatch, which makes it the probe
    /// for measuring dispatch length.
    Nop(),
    /// `acc = imm32`
    LdaSmi(Imm),
    /// `acc = r[a]`
    Ldar(Reg),
    /// `r[a] = acc`
    Star(Reg),
    /// `acc = r[a] + acc`
    Add(Reg),
    /// `acc = acc + 1`
    Inc(),
    /// `if acc < r[a] { pc += off32 }` (offset relative to this instruction)
    JumpIfLess(Reg, Off),
    /// `pc += off32` (offset relative to this instruction)
    Jump(Off),
    /// `acc = acc + imm32`. Like V8's `AddSmi`, it saves the operand fetch
    /// and its type check.
    AddSmi(Imm),
    /// `if acc < imm32 { pc += off32 }` (offset relative to this instruction)
    JumpIfLessSmi(Imm, Off),
    /// `Jump` backwards: a loop's back edge. Like V8's `JumpLoop`, the loop
    /// forms count iterations in the interpreter, to tier up hot loops into
    /// the JIT. The assembler emits them for jumps to already-bound labels.
    JumpLoop(Off),
    /// `JumpIfLess` backwards.
    JumpIfLessLoop(Reg, Off),
    /// `JumpIfLessSmi` backwards.
    JumpIfLessSmiLoop(Imm, Off),
    /// Return `acc` to the caller, or to the host from the outermost frame.
    Return(),

    // ---- Constants and registers ----
    /// `acc = undefined`
    LdaUndefined(),
    /// `acc = null`
    LdaNull(),
    /// `acc = true`
    LdaTrue(),
    /// `acc = false`
    LdaFalse(),
    /// `r[b] = r[a]`
    Mov(Reg, Reg),

    // ---- Arithmetic and comparisons ----
    /// `acc = acc % imm32`
    ModSmi(Imm),
    /// `acc = acc & imm32`
    BitAndSmi(Imm),
    /// `acc = acc | imm32`
    BitOrSmi(Imm),
    /// `acc = r[a] < acc`
    TestLessThan(Reg),
    /// `acc = r[a] == acc`
    TestEqual(Reg),
    /// `acc = acc == imm32`
    TestEqualSmi(Imm),

    // ---- Conditional jumps (forward only) ----
    /// `if acc === true { pc += off32 }`; `acc` must be a boolean.
    JumpIfTrue(Off),
    /// `if acc === false { pc += off32 }`; `acc` must be a boolean.
    JumpIfFalse(Off),
    /// `if ToBoolean(acc) { pc += off32 }`
    JumpIfToBooleanTrue(Off),
    /// `if !ToBoolean(acc) { pc += off32 }`
    JumpIfToBooleanFalse(Off),

    // ---- Properties ----
    /// `acc = global.name`, where the inline-cache slot names the property.
    /// A ReferenceError if there is no such property.
    LdaGlobal(Ic),
    /// `global.name = acc`
    StaGlobal(Ic),
    /// `acc = r[a].name`
    GetNamed(Reg, Ic),
    /// `r[a].name = acc`
    SetNamed(Reg, Ic),
    /// `acc = r[a][acc]`. The inline cache remembers the receiver's shape
    /// (its elements' type) for the JIT.
    GetKeyed(Reg, AutoIc),
    /// `r[a][r[b]] = acc`
    SetKeyed(Reg, Reg),

    // ---- Allocation ----
    /// `acc` = a new closure of nested function `f`.
    CreateClosure(Fn),
    /// `acc = []`
    CreateArray(),
    /// `acc = constants[k]`: a string, or a number that isn't an int32.
    LdaConstant(Const),
    /// `acc = {}`: a plain object with room for `n` properties in itself
    /// (a literal's count), whose shape the inline cache keeps.
    CreateObject(Count, AutoIc),

    // ---- Calls ----
    /// `acc = r[f](r[w+1], …, r[w+n])` with `this = r[w]`. The calls'
    /// inline cache remembers the last function called, for the JIT to
    /// inline.
    Call(Reg, Reg, Count, AutoIc),
    /// `acc = r[f](r[a], …, r[a+n-1])` with `this = undefined`.
    CallUndefined(Reg, Reg, Count, AutoIc),
    /// `acc = new r[f](r[a], …, r[a+n-1])`
    Construct(Reg, Reg, Count, AutoIc),

    // ---- Arithmetic, group 2 ----
    /// `acc = r[a] - acc`
    Sub(Reg),
    /// `acc = acc - imm32`
    SubSmi(Imm),
    /// `acc = r[a] * acc`
    Mul(Reg),
    /// `acc = acc * imm32`
    MulSmi(Imm),
    /// `acc = r[a] / acc`
    Div(Reg),
    /// `acc = acc / imm32`
    DivSmi(Imm),
    /// `acc = acc << imm32`
    ShiftLeftSmi(Imm),
    /// `acc = acc >> imm32`
    ShiftRightSmi(Imm),
    /// `acc = acc >>> imm32`
    ShiftRightLogicalSmi(Imm),

    // ---- Comparisons, group 2 ----
    /// `acc = r[a] > acc`
    TestGreaterThan(Reg),
    /// `acc = r[a] <= acc`
    TestLessThanOrEqual(Reg),
    /// `acc = r[a] >= acc`
    TestGreaterThanOrEqual(Reg),
    /// `acc = r[a] === acc`
    TestStrictEqual(Reg),
    /// `acc = acc === undefined`
    TestUndefined(),
    /// `acc = acc === null`
    TestNull(),
    /// `acc = acc == null` (null or undefined)
    TestNullish(),

    // ---- Compare-and-jumps, group 2: `acc OP r[a]` / `acc OP imm32` ----
    /// `if acc <= r[a] { pc += off32 }`
    JumpIfLessEqual(Reg, Off),
    /// `if acc > r[a] { pc += off32 }`
    JumpIfGreater(Reg, Off),
    /// `if acc >= r[a] { pc += off32 }`
    JumpIfGreaterEqual(Reg, Off),
    /// `if acc <= imm32 { pc += off32 }`
    JumpIfLessEqualSmi(Imm, Off),
    /// `if acc > imm32 { pc += off32 }`
    JumpIfGreaterSmi(Imm, Off),
    /// `if acc >= imm32 { pc += off32 }`
    JumpIfGreaterEqualSmi(Imm, Off),
    /// `JumpIfLessEqual` backwards.
    JumpIfLessEqualLoop(Reg, Off),
    /// `JumpIfGreater` backwards.
    JumpIfGreaterLoop(Reg, Off),
    /// `JumpIfGreaterEqual` backwards.
    JumpIfGreaterEqualLoop(Reg, Off),
    /// `JumpIfLessEqualSmi` backwards.
    JumpIfLessEqualSmiLoop(Imm, Off),
    /// `JumpIfGreaterSmi` backwards.
    JumpIfGreaterSmiLoop(Imm, Off),
    /// `JumpIfGreaterEqualSmi` backwards.
    JumpIfGreaterEqualSmiLoop(Imm, Off),

    // ---- Superinstructions (see `quicken`) ----
    // Each is a comparison immediately followed by a `JumpIfTrue` or
    // `JumpIfFalse`, which [`quicken`] fuses by rewriting the comparison's
    // opcode byte. The handler does both: it reads the jump's offset from
    // the instruction after it, and either jumps or skips it. The jump
    // instruction stays in place, so offsets and jump targets don't change,
    // and the accumulator ends as the comparison's boolean, as it would.
    /// `TestLessThan r` + `JumpIfTrue`.
    TestLessThanJumpIfTrue(Reg),
    /// `TestLessThan r` + `JumpIfFalse`.
    TestLessThanJumpIfFalse(Reg),
    /// `TestEqual r` + `JumpIfTrue`.
    TestEqualJumpIfTrue(Reg),
    /// `TestEqual r` + `JumpIfFalse`.
    TestEqualJumpIfFalse(Reg),
    /// `TestEqualSmi k` + `JumpIfTrue`.
    TestEqualSmiJumpIfTrue(Imm),
    /// `TestEqualSmi k` + `JumpIfFalse`.
    TestEqualSmiJumpIfFalse(Imm),
    TestGreaterThanJumpIfTrue(Reg),
    TestGreaterThanJumpIfFalse(Reg),
    TestLessThanOrEqualJumpIfTrue(Reg),
    TestLessThanOrEqualJumpIfFalse(Reg),
    TestGreaterThanOrEqualJumpIfTrue(Reg),
    TestGreaterThanOrEqualJumpIfFalse(Reg),
    TestStrictEqualJumpIfTrue(Reg),
    TestStrictEqualJumpIfFalse(Reg),
    TestUndefinedJumpIfTrue(),
    TestUndefinedJumpIfFalse(),
    TestNullJumpIfTrue(),
    TestNullJumpIfFalse(),
    TestNullishJumpIfTrue(),
    TestNullishJumpIfFalse(),
}

impl Op {
    /// Decode an opcode byte.
    pub fn from_u8(b: u8) -> Option<Op> {
        Self::ALL.get(b as usize).copied()
    }

    /// The opcode named `name` (its mnemonic).
    pub fn from_name(name: &str) -> Option<Op> {
        Self::ALL.iter().copied().find(|op| op.name() == name)
    }

    /// Total encoded length in bytes, opcode included.
    pub const fn size(self) -> usize {
        let ops = self.operands();
        let mut n = 1;
        let mut i = 0;
        while i < ops.len() {
            n += ops[i].width();
            i += 1;
        }
        n
    }

    /// The plain form of a loop opcode (`JumpLoop` → `Jump`, …) or of a
    /// superinstruction (`TestLessThanJumpIfTrue` → `TestLessThan`); any
    /// other opcode as is. They behave the same apart from counting
    /// iterations or dispatching once less.
    pub const fn plain(self) -> Op {
        match self {
            Op::JumpLoop => Op::Jump,
            Op::JumpIfLessLoop => Op::JumpIfLess,
            Op::JumpIfLessSmiLoop => Op::JumpIfLessSmi,
            Op::JumpIfLessEqualLoop => Op::JumpIfLessEqual,
            Op::JumpIfGreaterLoop => Op::JumpIfGreater,
            Op::JumpIfGreaterEqualLoop => Op::JumpIfGreaterEqual,
            Op::JumpIfLessEqualSmiLoop => Op::JumpIfLessEqualSmi,
            Op::JumpIfGreaterSmiLoop => Op::JumpIfGreaterSmi,
            Op::JumpIfGreaterEqualSmiLoop => Op::JumpIfGreaterEqualSmi,
            Op::TestGreaterThanJumpIfTrue | Op::TestGreaterThanJumpIfFalse => Op::TestGreaterThan,
            Op::TestLessThanOrEqualJumpIfTrue | Op::TestLessThanOrEqualJumpIfFalse => {
                Op::TestLessThanOrEqual
            }
            Op::TestGreaterThanOrEqualJumpIfTrue | Op::TestGreaterThanOrEqualJumpIfFalse => {
                Op::TestGreaterThanOrEqual
            }
            Op::TestStrictEqualJumpIfTrue | Op::TestStrictEqualJumpIfFalse => Op::TestStrictEqual,
            Op::TestUndefinedJumpIfTrue | Op::TestUndefinedJumpIfFalse => Op::TestUndefined,
            Op::TestNullJumpIfTrue | Op::TestNullJumpIfFalse => Op::TestNull,
            Op::TestNullishJumpIfTrue | Op::TestNullishJumpIfFalse => Op::TestNullish,
            Op::TestLessThanJumpIfTrue | Op::TestLessThanJumpIfFalse => Op::TestLessThan,
            Op::TestEqualJumpIfTrue | Op::TestEqualJumpIfFalse => Op::TestEqual,
            Op::TestEqualSmiJumpIfTrue | Op::TestEqualSmiJumpIfFalse => Op::TestEqualSmi,
            op => op,
        }
    }

    /// The superinstruction for `self` followed by `next`, if there is one.
    pub const fn fused_with(self, next: Op) -> Option<Op> {
        Some(match (self, next) {
            (Op::TestLessThan, Op::JumpIfTrue) => Op::TestLessThanJumpIfTrue,
            (Op::TestLessThan, Op::JumpIfFalse) => Op::TestLessThanJumpIfFalse,
            (Op::TestEqual, Op::JumpIfTrue) => Op::TestEqualJumpIfTrue,
            (Op::TestEqual, Op::JumpIfFalse) => Op::TestEqualJumpIfFalse,
            (Op::TestEqualSmi, Op::JumpIfTrue) => Op::TestEqualSmiJumpIfTrue,
            (Op::TestEqualSmi, Op::JumpIfFalse) => Op::TestEqualSmiJumpIfFalse,
            (Op::TestGreaterThan, Op::JumpIfTrue) => Op::TestGreaterThanJumpIfTrue,
            (Op::TestGreaterThan, Op::JumpIfFalse) => Op::TestGreaterThanJumpIfFalse,
            (Op::TestLessThanOrEqual, Op::JumpIfTrue) => Op::TestLessThanOrEqualJumpIfTrue,
            (Op::TestLessThanOrEqual, Op::JumpIfFalse) => Op::TestLessThanOrEqualJumpIfFalse,
            (Op::TestGreaterThanOrEqual, Op::JumpIfTrue) => Op::TestGreaterThanOrEqualJumpIfTrue,
            (Op::TestGreaterThanOrEqual, Op::JumpIfFalse) => Op::TestGreaterThanOrEqualJumpIfFalse,
            (Op::TestStrictEqual, Op::JumpIfTrue) => Op::TestStrictEqualJumpIfTrue,
            (Op::TestStrictEqual, Op::JumpIfFalse) => Op::TestStrictEqualJumpIfFalse,
            (Op::TestUndefined, Op::JumpIfTrue) => Op::TestUndefinedJumpIfTrue,
            (Op::TestUndefined, Op::JumpIfFalse) => Op::TestUndefinedJumpIfFalse,
            (Op::TestNull, Op::JumpIfTrue) => Op::TestNullJumpIfTrue,
            (Op::TestNull, Op::JumpIfFalse) => Op::TestNullJumpIfFalse,
            (Op::TestNullish, Op::JumpIfTrue) => Op::TestNullishJumpIfTrue,
            (Op::TestNullish, Op::JumpIfFalse) => Op::TestNullishJumpIfFalse,
            _ => return None,
        })
    }

    /// The loop form of a jump opcode.
    const fn looping(self) -> Op {
        match self {
            Op::Jump => Op::JumpLoop,
            Op::JumpIfLess => Op::JumpIfLessLoop,
            Op::JumpIfLessSmi => Op::JumpIfLessSmiLoop,
            Op::JumpIfLessEqual => Op::JumpIfLessEqualLoop,
            Op::JumpIfGreater => Op::JumpIfGreaterLoop,
            Op::JumpIfGreaterEqual => Op::JumpIfGreaterEqualLoop,
            Op::JumpIfLessEqualSmi => Op::JumpIfLessEqualSmiLoop,
            Op::JumpIfGreaterSmi => Op::JumpIfGreaterSmiLoop,
            Op::JumpIfGreaterEqualSmi => Op::JumpIfGreaterEqualSmiLoop,
            op => op,
        }
    }
}

/// Fuse comparison + conditional-jump pairs into superinstructions, in
/// place (see the superinstruction opcodes). Only opcode bytes change, so
/// the result is equivalent instruction by instruction; a pair whose jump
/// is also a jump target still works, as the jump stays. Idempotent.
///
/// # Panics
/// On an invalid opcode or a truncated instruction.
pub fn quicken(code: &mut [u8]) {
    let mut pc = 0;
    while pc < code.len() {
        let op = Op::from_u8(code[pc]).expect("bad opcode").plain();
        let next = pc + op.size();
        assert!(next <= code.len(), "truncated instruction at {pc}");
        if next < code.len()
            && let Some(q) = op.fused_with(Op::from_u8(code[next]).expect("bad opcode"))
        {
            code[pc] = q as u8;
        }
        pc = next;
    }
}

/// An operand value for [`Assembler::emit`].
#[derive(Clone, Copy, Debug)]
pub enum Arg {
    Reg(u8),
    Imm(i32),
    Label(Label),
    Index(u16),
    Count(u8),
}

/// A tiny assembler with backpatched labels.
#[derive(Default, Clone)]
pub struct Assembler {
    code: Vec<u8>,
    labels: Vec<Option<usize>>,
    /// (position of the offset field, label, start of the instruction)
    fixups: Vec<(usize, usize, usize)>,
}

#[derive(Clone, Copy, Debug)]
pub struct Label(usize);

impl Assembler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn label(&mut self) -> Label {
        self.labels.push(None);
        Label(self.labels.len() - 1)
    }

    pub fn bind(&mut self, l: Label) {
        self.labels[l.0] = Some(self.code.len());
    }

    fn op(&mut self, op: Op) -> &mut Self {
        self.code.push(op as u8);
        self
    }

    fn reg(&mut self, r: u8) -> &mut Self {
        self.code.push(r);
        self
    }

    fn imm(&mut self, v: i32) -> &mut Self {
        self.code.extend_from_slice(&v.to_le_bytes());
        self
    }

    /// Emit a placeholder offset for the jump instruction that starts at `start`.
    fn jump_to(&mut self, start: usize, l: Label) {
        let at = self.code.len();
        self.code.extend_from_slice(&0i32.to_le_bytes());
        self.fixups.push((at, l.0, start));
    }

    pub fn nop(&mut self) -> &mut Self {
        self.op(Op::Nop)
    }
    pub fn lda_smi(&mut self, v: i32) -> &mut Self {
        self.op(Op::LdaSmi).imm(v)
    }
    pub fn ldar(&mut self, r: u8) -> &mut Self {
        self.op(Op::Ldar).reg(r)
    }
    pub fn star(&mut self, r: u8) -> &mut Self {
        self.op(Op::Star).reg(r)
    }
    pub fn add(&mut self, r: u8) -> &mut Self {
        self.op(Op::Add).reg(r)
    }
    pub fn inc(&mut self) -> &mut Self {
        self.op(Op::Inc)
    }
    /// `op`, or its loop form if `l` is already bound (a backward jump).
    fn jump_op(&self, op: Op, l: Label) -> Op {
        if self.labels[l.0].is_some() {
            op.looping()
        } else {
            op
        }
    }

    pub fn jump_if_less(&mut self, r: u8, l: Label) -> &mut Self {
        let start = self.code.len();
        let op = self.jump_op(Op::JumpIfLess, l);
        self.op(op).reg(r).jump_to(start, l);
        self
    }
    pub fn add_smi(&mut self, v: i32) -> &mut Self {
        self.op(Op::AddSmi).imm(v)
    }
    pub fn jump_if_less_smi(&mut self, v: i32, l: Label) -> &mut Self {
        let start = self.code.len();
        let op = self.jump_op(Op::JumpIfLessSmi, l);
        self.op(op).imm(v).jump_to(start, l);
        self
    }
    pub fn jump(&mut self, l: Label) -> &mut Self {
        let start = self.code.len();
        let op = self.jump_op(Op::Jump, l);
        self.op(op).jump_to(start, l);
        self
    }
    pub fn ret(&mut self) -> &mut Self {
        self.op(Op::Return)
    }

    /// Emit `op` with `args`, which must match its operand kinds. A jump to
    /// an already-bound label takes the loop form; only `Jump` and the
    /// compare-and-jumps (`JumpIfLess`, `JumpIfGreaterEqualSmi`, …) have one.
    pub fn emit(&mut self, op: Op, args: &[Arg]) -> &mut Self {
        let kinds = op.operands();
        assert_eq!(
            kinds.len(),
            args.len(),
            "{op:?} takes {} operands",
            kinds.len()
        );
        let start = self.code.len();
        let mut op = op.plain();
        for a in args {
            if let Arg::Label(l) = *a
                && self.labels[l.0].is_some()
            {
                assert_ne!(op.looping(), op, "{op:?} cannot jump backwards");
                op = op.looping();
            }
        }
        self.op(op);
        for (k, a) in kinds.iter().zip(args) {
            match (k, *a) {
                (Operand::Reg, Arg::Reg(r)) => {
                    self.reg(r);
                }
                (Operand::Count, Arg::Count(n)) => {
                    self.reg(n);
                }
                (Operand::Imm, Arg::Imm(v)) => {
                    self.imm(v);
                }
                (Operand::Off, Arg::Label(l)) => self.jump_to(start, l),
                (Operand::Ic | Operand::Fn | Operand::AutoIc | Operand::Const, Arg::Index(i)) => {
                    self.code.extend_from_slice(&i.to_le_bytes());
                }
                (k, a) => panic!("{op:?}: operand {a:?} is not a {k:?}"),
            }
        }
        self
    }

    pub fn finish(mut self) -> Vec<u8> {
        for &(at, label, start) in &self.fixups {
            let target = self.labels[label].expect("unbound label");
            let off = target as i64 - start as i64;
            self.code[at..at + 4].copy_from_slice(&(off as i32).to_le_bytes());
        }
        self.code
    }
}

#[cfg(test)]
mod tests {
    use super::{Arg, Assembler, Op};

    #[test]
    fn from_u8_round_trips() {
        for (i, op) in Op::ALL.iter().enumerate() {
            assert_eq!(*op as usize, i);
            assert_eq!(Op::from_u8(i as u8), Some(*op));
        }
        assert_eq!(Op::from_u8(Op::COUNT as u8), None);
        for op in Op::ALL {
            assert_eq!(Op::from_name(op.name()), Some(op));
        }
    }

    #[test]
    fn sizes_match_the_original_encodings() {
        assert_eq!(Op::Return.size(), 1);
        assert_eq!(Op::Ldar.size(), 2);
        assert_eq!(Op::LdaSmi.size(), 5);
        assert_eq!(Op::JumpIfLess.size(), 6);
        assert_eq!(Op::JumpIfLessSmi.size(), 9);
        assert_eq!(Op::GetNamed.size(), 4);
        assert_eq!(Op::Call.size(), 6);
    }

    #[test]
    fn quicken_rewrites_only_opcode_bytes() {
        let mut a = Assembler::new();
        let out = a.label();
        a.emit(Op::TestLessThan, &[Arg::Reg(1)]);
        a.emit(Op::JumpIfFalse, &[Arg::Label(out)]);
        a.emit(Op::TestEqualSmi, &[Arg::Imm(3)]);
        a.emit(Op::Return, &[]);
        a.bind(out);
        a.emit(Op::Return, &[]);
        let plain = a.finish();
        let mut q = plain.clone();
        super::quicken(&mut q);
        assert_eq!(q[0], Op::TestLessThanJumpIfFalse as u8);
        assert_eq!(q[2..], plain[2..]);
        let again = q.clone();
        super::quicken(&mut q);
        assert_eq!(q, again);
        for op in Op::ALL {
            assert_eq!(op.size(), op.plain().size(), "{op:?}");
        }
    }

    #[test]
    fn emit_picks_loop_forms_for_backward_jumps() {
        let mut a = Assembler::new();
        let top = a.label();
        let out = a.label();
        a.bind(top);
        a.emit(Op::JumpIfLessSmi, &[Arg::Imm(3), Arg::Label(out)]);
        a.emit(Op::JumpIfLessSmi, &[Arg::Imm(3), Arg::Label(top)]);
        a.bind(out);
        a.emit(Op::Return, &[]);
        let code = a.finish();
        assert_eq!(code[0], Op::JumpIfLessSmi as u8);
        assert_eq!(code[9], Op::JumpIfLessSmiLoop as u8);
        assert_eq!(i32::from_le_bytes(code[14..18].try_into().unwrap()), -9);
    }
}
