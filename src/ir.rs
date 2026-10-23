//! The intermediate representation: each function as a control-flow graph over places.
//!
//! A function has typed locals (storage slots) and basic blocks. Each block is a list of
//! statements, which compute a value and assign it to a place, followed by a terminator, which
//! decides where control goes next. See "Lowering" in design/compiler.md.

use std::fmt::Write;

use crate::hir::Type;
use crate::intern::{Interner, Symbol};

pub struct Ir {
    pub functions: Vec<Function>,
}

pub struct Function {
    pub name: Symbol,
    /// Local `_0` is always the return value.
    pub locals: Vec<LocalDecl>,
    /// Execution starts at block `bb0`.
    pub blocks: Vec<Block>,
}

/// Refers to a local within its function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Local(pub u32);

impl Local {
    /// The local holding a function's return value.
    pub const RETURN: Local = Local(0);
}

#[derive(Clone, Copy, Debug)]
pub struct LocalDecl {
    pub ty: Type,
    /// The variable this local holds, or `None` for the return value and temporaries.
    pub name: Option<Symbol>,
}

pub struct Block {
    pub statements: Vec<Statement>,
    pub terminator: Terminator,
}

#[derive(Clone, Copy, Debug)]
pub enum Statement {
    Assign { place: Place, value: Rvalue },
}

/// A location that can be read or assigned. For now only a whole local; later also parts of one,
/// like fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Place {
    pub local: Local,
}

/// A value used by an rvalue.
#[derive(Clone, Copy, Debug)]
pub enum Operand {
    Constant { value: i128, ty: Type },
    /// The value in a place, which is left unchanged. Moving a value out of a place, which linear
    /// types will need, will be a separate operand.
    Copy(Place),
}

/// A computation whose result is stored in a place. The result has the place's type.
///
/// Some rvalues can trap. A trap ends the process immediately, so it doesn't need an edge in the
/// control-flow graph: nothing runs after it.
#[derive(Clone, Copy, Debug)]
pub enum Rvalue {
    Use(Operand),
    /// Arithmetic on two operands of the result's type. If `checked`, traps on overflow; otherwise
    /// wraps. Division and remainder are always checked, and also trap on division by zero.
    Binary { op: BinOp, checked: bool, lhs: Operand, rhs: Operand },
    /// Negation of a signed operand. Traps on overflow if `checked`; otherwise wraps.
    Neg { checked: bool, operand: Operand },
    /// Converts an integer operand to the result's type. If `checked`, traps when the value
    /// doesn't fit; otherwise keeps the low bits.
    Cast { checked: bool, operand: Operand },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

#[derive(Clone, Copy, Debug)]
pub enum Terminator {
    /// Returns from the function. The return value is whatever is in `_0`.
    Return,
}

impl Ir {
    /// Formats the IR for `--emit=ir`.
    pub fn dump(&self, symbols: &Interner) -> String {
        let mut out = String::new();
        for function in &self.functions {
            let return_type = function.locals[Local::RETURN.0 as usize].ty;
            writeln!(out, "fn {}() -> {return_type} {{", symbols.get(function.name)).unwrap();
            for (i, local) in function.locals.iter().enumerate() {
                match local.name {
                    Some(name) => writeln!(out, "    let _{i}: {}  // {}", local.ty, symbols.get(name)).unwrap(),
                    None => writeln!(out, "    let _{i}: {}", local.ty).unwrap(),
                }
            }
            for (i, block) in function.blocks.iter().enumerate() {
                writeln!(out, "  bb{i}:").unwrap();
                for statement in &block.statements {
                    match statement {
                        Statement::Assign { place, value } => {
                            let ty = function.locals[place.local.0 as usize].ty;
                            writeln!(out, "    {} = {}", dump_place(*place), dump_rvalue(*value, ty)).unwrap();
                        }
                    }
                }
                match block.terminator {
                    Terminator::Return => writeln!(out, "    return").unwrap(),
                }
            }
            writeln!(out, "}}").unwrap();
        }
        out
    }
}

fn dump_place(place: Place) -> String {
    format!("_{}", place.local.0)
}

fn dump_operand(operand: Operand) -> String {
    match operand {
        Operand::Constant { value, ty } => format!("const {value}_{ty}"),
        Operand::Copy(place) => format!("copy {}", dump_place(place)),
    }
}

/// Formats an rvalue whose result has type `ty`.
fn dump_rvalue(rvalue: Rvalue, ty: Type) -> String {
    let checked = |checked| if checked { "checked" } else { "wrapping" };
    match rvalue {
        Rvalue::Use(operand) => dump_operand(operand),
        Rvalue::Binary { op, checked: is_checked, lhs, rhs } => {
            let op = match op {
                BinOp::Add => "add",
                BinOp::Sub => "sub",
                BinOp::Mul => "mul",
                BinOp::Div => "div",
                BinOp::Rem => "rem",
            };
            format!("{}_{op}({}, {})", checked(is_checked), dump_operand(lhs), dump_operand(rhs))
        }
        Rvalue::Neg { checked: is_checked, operand } => format!("{}_neg({})", checked(is_checked), dump_operand(operand)),
        Rvalue::Cast { checked: is_checked, operand } => {
            format!("{}_cast({}) to {ty}", checked(is_checked), dump_operand(operand))
        }
    }
}
