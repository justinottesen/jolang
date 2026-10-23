//! x86-64 instructions, and printing them as GNU assembler source.
//!
//! Instructions are kept structured rather than generated as text, so that an assembler written
//! later can encode the same instructions to machine code directly.

use std::fmt::Write;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reg {
    Rax,
    Rdi,
    Rbp,
    Rsp,
}

/// How many bytes of a register or memory location an operand covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Size {
    Byte,
    Dword,
    Qword,
}

impl Size {
    pub fn bytes(self) -> u32 {
        match self {
            Size::Byte => 1,
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

#[derive(Clone, Debug)]
pub enum Inst {
    Label(String),
    Mov { dst: Operand, src: Operand },
    /// Move with zero extension, from a smaller operand to a larger register.
    Movzx { dst: Operand, src: Operand },
    Sub { dst: Operand, src: Operand },
    Push(Reg),
    Pop(Reg),
    Call(String),
    Ret,
    Syscall,
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
    let text = match inst {
        Inst::Label(name) => {
            writeln!(out, "{name}:").unwrap();
            return;
        }
        Inst::Mov { dst, src } => format!("mov {}, {}", operand(*dst), operand(*src)),
        Inst::Movzx { dst, src } => format!("movzx {}, {}", operand(*dst), operand(*src)),
        Inst::Sub { dst, src } => format!("sub {}, {}", operand(*dst), operand(*src)),
        Inst::Push(reg) => format!("push {}", reg_name(*reg, Size::Qword)),
        Inst::Pop(reg) => format!("pop {}", reg_name(*reg, Size::Qword)),
        Inst::Call(target) => format!("call {target}"),
        Inst::Ret => "ret".to_string(),
        Inst::Syscall => "syscall".to_string(),
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
    match (reg, size) {
        (Reg::Rax, Size::Byte) => "al",
        (Reg::Rax, Size::Dword) => "eax",
        (Reg::Rax, Size::Qword) => "rax",
        (Reg::Rdi, Size::Byte) => "dil",
        (Reg::Rdi, Size::Dword) => "edi",
        (Reg::Rdi, Size::Qword) => "rdi",
        (Reg::Rbp, Size::Byte) => "bpl",
        (Reg::Rbp, Size::Dword) => "ebp",
        (Reg::Rbp, Size::Qword) => "rbp",
        (Reg::Rsp, Size::Byte) => "spl",
        (Reg::Rsp, Size::Dword) => "esp",
        (Reg::Rsp, Size::Qword) => "rsp",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operands() {
        assert_eq!(operand(Operand::Reg(Reg::Rdi, Size::Byte)), "dil");
        assert_eq!(operand(Operand::Imm(-3)), "-3");
        let mem = |offset| Operand::Mem { base: Reg::Rbp, offset, size: Size::Byte };
        assert_eq!(operand(mem(0)), "byte ptr [rbp]");
        assert_eq!(operand(mem(-1)), "byte ptr [rbp - 1]");
        assert_eq!(operand(mem(16)), "byte ptr [rbp + 16]");
    }
}
