//! x86-64 instructions, and printing them as GNU assembler source.
//!
//! Instructions are kept structured rather than generated as text, so that an assembler written
//! later can encode the same instructions to machine code directly.

use std::fmt::Write;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reg {
    Rax,
    Rcx,
    Rdx,
    Rdi,
    Rbp,
    Rsp,
}

/// How many bytes of a register or memory location an operand covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Size {
    Byte,
    Word,
    Dword,
    Qword,
}

impl Size {
    pub fn bytes(self) -> u32 {
        match self {
            Size::Byte => 1,
            Size::Word => 2,
            Size::Dword => 4,
            Size::Qword => 8,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Operand {
    Reg(Reg, Size),
    Imm(i64),
    /// The memory at `[base + offset]`.
    Mem { base: Reg, offset: i32, size: Size },
}

/// A condition for a conditional jump, based on the flags set by the previous instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cond {
    /// Unsigned greater than.
    Above,
    /// The carry flag: unsigned overflow.
    Carry,
    Equal,
    NotEqual,
    /// Signed greater than.
    Greater,
    /// Signed less than.
    Less,
    /// The overflow flag: signed overflow.
    Overflow,
    /// The sign flag: the result was negative.
    Sign,
}

#[derive(Clone, Debug)]
pub enum Inst {
    Label(String),
    Mov { dst: Operand, src: Operand },
    /// Move with zero extension, from a smaller operand to a larger register.
    Movzx { dst: Operand, src: Operand },
    /// Move with sign extension, from a smaller operand to a larger register.
    Movsx { dst: Operand, src: Operand },
    Add { dst: Operand, src: Operand },
    Sub { dst: Operand, src: Operand },
    /// Signed multiply, keeping the low half of the product. The low half is the same for signed
    /// and unsigned operands; only the overflow flag is signed.
    Imul { dst: Operand, src: Operand },
    /// Unsigned multiply of `rax` by the operand, into `rdx:rax`.
    Mul(Operand),
    /// Unsigned divide of `rdx:rax` by the operand: quotient in `rax`, remainder in `rdx`.
    Div(Operand),
    /// Signed divide of `rdx:rax` by the operand: quotient in `rax`, remainder in `rdx`.
    Idiv(Operand),
    Neg(Operand),
    Cmp { lhs: Operand, rhs: Operand },
    Test { lhs: Operand, rhs: Operand },
    Xor { dst: Operand, src: Operand },
    /// Sign-extends `eax` into `edx`, for a 32-bit `idiv`.
    Cdq,
    /// Sign-extends `rax` into `rdx`, for a 64-bit `idiv`.
    Cqo,
    Push(Reg),
    Pop(Reg),
    Call(String),
    /// Jumps to the label if the condition holds.
    Jcc(Cond, String),
    Ret,
    Syscall,
    /// An undefined instruction, which makes the CPU raise an exception. Linux kills the process
    /// with `SIGILL`.
    Ud2,
}

pub struct Function {
    pub name: String,
    /// Whether the symbol is visible to the linker.
    pub global: bool,
    pub insts: Vec<Inst>,
}

/// Prints functions as a complete assembly file, in Intel syntax.
pub fn print(functions: &[Function]) -> String {
    let mut out = String::new();
    out += "    .intel_syntax noprefix\n";
    out += "    .text\n";
    for function in functions {
        out += "\n";
        if function.global {
            writeln!(out, "    .globl {}", function.name).unwrap();
        }
        writeln!(out, "{}:", function.name).unwrap();
        for inst in &function.insts {
            print_inst(&mut out, inst);
        }
    }
    out
}

fn print_inst(out: &mut String, inst: &Inst) {
    let two = |name: &str, a: &Operand, b: &Operand| format!("{name} {}, {}", operand(*a), operand(*b));
    let text = match inst {
        Inst::Label(name) => {
            writeln!(out, "{name}:").unwrap();
            return;
        }
        Inst::Mov { dst, src } => two("mov", dst, src),
        Inst::Movzx { dst, src } => two("movzx", dst, src),
        // Sign-extending from 32 bits has its own mnemonic.
        Inst::Movsx { dst, src: src @ (Operand::Mem { size: Size::Dword, .. } | Operand::Reg(_, Size::Dword)) } => {
            two("movsxd", dst, src)
        }
        Inst::Movsx { dst, src } => two("movsx", dst, src),
        Inst::Add { dst, src } => two("add", dst, src),
        Inst::Sub { dst, src } => two("sub", dst, src),
        Inst::Imul { dst, src } => two("imul", dst, src),
        Inst::Mul(src) => format!("mul {}", operand(*src)),
        Inst::Div(src) => format!("div {}", operand(*src)),
        Inst::Idiv(src) => format!("idiv {}", operand(*src)),
        Inst::Neg(dst) => format!("neg {}", operand(*dst)),
        Inst::Cmp { lhs, rhs } => two("cmp", lhs, rhs),
        Inst::Test { lhs, rhs } => two("test", lhs, rhs),
        Inst::Xor { dst, src } => two("xor", dst, src),
        Inst::Cdq => "cdq".to_string(),
        Inst::Cqo => "cqo".to_string(),
        Inst::Push(reg) => format!("push {}", reg_name(*reg, Size::Qword)),
        Inst::Pop(reg) => format!("pop {}", reg_name(*reg, Size::Qword)),
        Inst::Call(target) => format!("call {target}"),
        Inst::Jcc(cond, target) => {
            let suffix = match cond {
                Cond::Above => "a",
                Cond::Carry => "c",
                Cond::Equal => "e",
                Cond::NotEqual => "ne",
                Cond::Greater => "g",
                Cond::Less => "l",
                Cond::Overflow => "o",
                Cond::Sign => "s",
            };
            format!("j{suffix} {target}")
        }
        Inst::Ret => "ret".to_string(),
        Inst::Syscall => "syscall".to_string(),
        Inst::Ud2 => "ud2".to_string(),
    };
    writeln!(out, "    {text}").unwrap();
}

fn operand(operand: Operand) -> String {
    match operand {
        Operand::Reg(reg, size) => reg_name(reg, size).to_string(),
        Operand::Imm(value) => value.to_string(),
        Operand::Mem { base, offset, size } => {
            let size = match size {
                Size::Byte => "byte",
                Size::Word => "word",
                Size::Dword => "dword",
                Size::Qword => "qword",
            };
            let base = reg_name(base, Size::Qword);
            match offset {
                0 => format!("{size} ptr [{base}]"),
                ..0 => format!("{size} ptr [{base} - {}]", offset.unsigned_abs()),
                _ => format!("{size} ptr [{base} + {offset}]"),
            }
        }
    }
}

fn reg_name(reg: Reg, size: Size) -> &'static str {
    let names = match reg {
        Reg::Rax => ["al", "ax", "eax", "rax"],
        Reg::Rcx => ["cl", "cx", "ecx", "rcx"],
        Reg::Rdx => ["dl", "dx", "edx", "rdx"],
        Reg::Rdi => ["dil", "di", "edi", "rdi"],
        Reg::Rbp => ["bpl", "bp", "ebp", "rbp"],
        Reg::Rsp => ["spl", "sp", "esp", "rsp"],
    };
    match size {
        Size::Byte => names[0],
        Size::Word => names[1],
        Size::Dword => names[2],
        Size::Qword => names[3],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operands() {
        assert_eq!(operand(Operand::Reg(Reg::Rdi, Size::Byte)), "dil");
        assert_eq!(operand(Operand::Reg(Reg::Rcx, Size::Word)), "cx");
        assert_eq!(operand(Operand::Imm(-3)), "-3");
        let mem = |offset| Operand::Mem { base: Reg::Rbp, offset, size: Size::Byte };
        assert_eq!(operand(mem(0)), "byte ptr [rbp]");
        assert_eq!(operand(mem(-1)), "byte ptr [rbp - 1]");
        assert_eq!(operand(mem(16)), "byte ptr [rbp + 16]");
    }

    #[test]
    fn sign_extension_from_32_bits() {
        let mut out = String::new();
        let dst = Operand::Reg(Reg::Rax, Size::Qword);
        print_inst(&mut out, &Inst::Movsx { dst, src: Operand::Mem { base: Reg::Rbp, offset: -4, size: Size::Dword } });
        print_inst(&mut out, &Inst::Movsx { dst, src: Operand::Mem { base: Reg::Rbp, offset: -1, size: Size::Byte } });
        assert_eq!(out, "    movsxd rax, dword ptr [rbp - 4]\n    movsx rax, byte ptr [rbp - 1]\n");
    }
}
