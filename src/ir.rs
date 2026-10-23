//! The intermediate representation: each function as a control-flow graph over places.
//!
//! A function has typed locals (storage slots) and basic blocks. Each block is a list of
//! statements, which assign values to places, followed by a terminator, which decides where control
//! goes next. See "Lowering" in design/compiler.md.

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
}

pub struct Block {
    pub statements: Vec<Statement>,
    pub terminator: Terminator,
}

#[derive(Clone, Copy, Debug)]
pub enum Statement {
    Assign { place: Place, value: Operand },
}

/// A location that can be read or assigned. For now only a whole local; later also parts of one,
/// like fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Place {
    pub local: Local,
}

/// A value used by a statement.
#[derive(Clone, Copy, Debug)]
pub enum Operand {
    Constant { value: u128, ty: Type },
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
                writeln!(out, "    let _{i}: {}", local.ty).unwrap();
            }
            for (i, block) in function.blocks.iter().enumerate() {
                writeln!(out, "  bb{i}:").unwrap();
                for statement in &block.statements {
                    match statement {
                        Statement::Assign { place, value } => {
                            writeln!(out, "    {} = {}", dump_place(*place), dump_operand(*value)).unwrap();
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
    }
}
