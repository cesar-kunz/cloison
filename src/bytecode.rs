//! Bytecode format: an accumulator + register machine (in the spirit of V8's
//! Ignition). Instructions are variable length: a one-byte opcode followed by
//! its operands, little-endian, unaligned.

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    /// No operation. Its handler is pure dispatch, which makes it the probe
    /// for measuring dispatch length.
    Nop,
    /// `acc = imm32`
    LdaSmi,
    /// `acc = r[a]`
    Ldar,
    /// `r[a] = acc`
    Star,
    /// `acc = acc + r[a]`
    Add,
    /// `acc = acc + 1`
    Inc,
    /// `if acc < r[a] { pc += off32 }` (offset relative to this instruction)
    JumpIfLess,
    /// `pc += off32` (offset relative to this instruction)
    Jump,
    /// `acc = acc + imm32`. Like V8's `AddSmi`, it saves the operand fetch
    /// and its type check.
    AddSmi,
    /// `if acc < imm32 { pc += off32 }` (offset relative to this instruction)
    JumpIfLessSmi,
    /// Return `acc` to the host.
    Return,
}

impl Op {
    pub const COUNT: usize = Op::Return as usize + 1;

    /// Total encoded length in bytes, opcode included.
    pub const fn size(self) -> usize {
        match self {
            Op::Nop | Op::Inc | Op::Return => 1,
            Op::Ldar | Op::Star | Op::Add => 2,
            Op::LdaSmi | Op::Jump | Op::AddSmi => 5,
            Op::JumpIfLess => 6,
            Op::JumpIfLessSmi => 9,
        }
    }
}

/// A tiny assembler with backpatched labels.
#[derive(Default)]
pub struct Assembler {
    code: Vec<u8>,
    labels: Vec<Option<usize>>,
    /// (position of the offset field, label, start of the instruction)
    fixups: Vec<(usize, usize, usize)>,
}

#[derive(Clone, Copy)]
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
    pub fn jump_if_less(&mut self, r: u8, l: Label) -> &mut Self {
        let start = self.code.len();
        self.op(Op::JumpIfLess).reg(r).jump_to(start, l);
        self
    }
    pub fn add_smi(&mut self, v: i32) -> &mut Self {
        self.op(Op::AddSmi).imm(v)
    }
    pub fn jump_if_less_smi(&mut self, v: i32, l: Label) -> &mut Self {
        let start = self.code.len();
        self.op(Op::JumpIfLessSmi).imm(v).jump_to(start, l);
        self
    }
    pub fn jump(&mut self, l: Label) -> &mut Self {
        let start = self.code.len();
        self.op(Op::Jump).jump_to(start, l);
        self
    }
    pub fn ret(&mut self) -> &mut Self {
        self.op(Op::Return)
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
