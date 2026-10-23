//! Generates x86-64 instructions from the IR, for Linux.
//!
//! Every local gets its own slot in the stack frame, and each statement reads and writes those
//! slots directly. This is simple, and makes slow code. See "Codegen" in design/compiler.md.

use crate::asm::{self, Inst, Operand, Reg, Size};
use crate::hir::Type;
use crate::intern::Interner;
use crate::ir::{self, Ir, Local, LocalDecl, Place, Statement, Terminator};

/// Generates the whole program: an entry point, plus every function in the IR.
pub fn generate(ir: &Ir, symbols: &Interner) -> Vec<asm::Function> {
    let mut functions = vec![linux_entry_point()];
    functions.extend(ir.functions.iter().map(|function| generate_function(function, symbols)));
    functions
}

/// The program's entry point on Linux, `_start`. It calls `main`, then exits the process with
/// `main`'s return value as the exit status. Linux starts it with `rsp` 16-byte aligned, so after
/// `call` pushes the return address, `main` starts with the alignment the System V ABI expects.
fn linux_entry_point() -> asm::Function {
    const SYS_EXIT_GROUP: i64 = 231;
    let insts = vec![
        Inst::Call("main".into()),
        // `main` returns a `u8` in `al`; the rest of `rax` is unspecified.
        Inst::Movzx { dst: Operand::Reg(Reg::Rdi, Size::Dword), src: Operand::Reg(Reg::Rax, Size::Byte) },
        Inst::Mov { dst: Operand::Reg(Reg::Rax, Size::Dword), src: Operand::Imm(SYS_EXIT_GROUP) },
        Inst::Syscall,
    ];
    asm::Function { name: "_start".into(), global: true, insts }
}

/// The size of a value of type `ty` in a register or memory operand. `()` has no size, since it
/// has no data, so no instruction ever reads or writes one.
fn operand_size(ty: Type) -> Option<Size> {
    match ty {
        Type::Unit => None,
        Type::U8 => Some(Size::Byte),
    }
}

/// Where each local lives in a function's stack frame.
struct Frame {
    /// Offset of each local's slot from `rbp`, indexed by local. `None` for a local that takes no
    /// space.
    offsets: Vec<Option<i32>>,
    /// Total size of the frame. A multiple of 16, to keep `rsp` aligned for calls.
    size: u32,
}

fn layout_frame(locals: &[LocalDecl]) -> Frame {
    let mut used = 0;
    let mut offsets = Vec::new();
    for local in locals {
        let Some(size) = operand_size(local.ty) else {
            offsets.push(None);
            continue;
        };
        // Slots grow downward from `rbp`. Every type's alignment is currently its size.
        let bytes = size.bytes();
        used = (used + bytes).next_multiple_of(bytes);
        offsets.push(Some(-(used as i32)));
    }
    Frame { offsets, size: used.next_multiple_of(16) }
}

fn generate_function(function: &ir::Function, symbols: &Interner) -> asm::Function {
    let name = symbols.get(function.name);
    let frame = layout_frame(&function.locals);
    // The memory operand for a place's slot. Lowering never reads or writes a place of type `()`.
    let slot = |place: Place| {
        let local = place.local.0 as usize;
        let offset = frame.offsets[local].expect("access to a zero-sized local");
        let size = operand_size(function.locals[local].ty).expect("access to a zero-sized local");
        Operand::Mem { base: Reg::Rbp, offset, size }
    };

    // Prologue: save the caller's frame pointer, and reserve space for this frame.
    let mut insts = vec![
        Inst::Push(Reg::Rbp),
        Inst::Mov { dst: Operand::Reg(Reg::Rbp, Size::Qword), src: Operand::Reg(Reg::Rsp, Size::Qword) },
    ];
    if frame.size > 0 {
        let size = Operand::Imm(frame.size.into());
        insts.push(Inst::Sub { dst: Operand::Reg(Reg::Rsp, Size::Qword), src: size });
    }

    for (i, block) in function.blocks.iter().enumerate() {
        insts.push(Inst::Label(format!(".L{name}.bb{i}")));
        for statement in &block.statements {
            match *statement {
                Statement::Assign { place, value: ir::Operand::Constant { value, .. } } => {
                    insts.push(Inst::Mov { dst: slot(place), src: immediate(value) });
                }
                Statement::Assign { place, value: ir::Operand::Copy(source) } => {
                    // x86 can't move directly from memory to memory, so go through `rax`.
                    let Operand::Mem { size, .. } = slot(source) else { unreachable!() };
                    let rax = Operand::Reg(Reg::Rax, size);
                    insts.push(Inst::Mov { dst: rax, src: slot(source) });
                    insts.push(Inst::Mov { dst: slot(place), src: rax });
                }
            }
        }
        match block.terminator {
            Terminator::Return => {
                let return_type = function.locals[Local::RETURN.0 as usize].ty;
                if let Some(size) = operand_size(return_type) {
                    let return_reg = Operand::Reg(Reg::Rax, size);
                    insts.push(Inst::Mov { dst: return_reg, src: slot(Place { local: Local::RETURN }) });
                }
                // Epilogue: free the frame, and restore the caller's frame pointer.
                insts.push(Inst::Mov {
                    dst: Operand::Reg(Reg::Rsp, Size::Qword),
                    src: Operand::Reg(Reg::Rbp, Size::Qword),
                });
                insts.push(Inst::Pop(Reg::Rbp));
                insts.push(Inst::Ret);
            }
        }
    }

    asm::Function { name: name.to_string(), global: false, insts }
}

fn immediate(value: u128) -> Operand {
    // Every constant is a `u8` for now. Wider ones will need care: most instructions only take
    // 32-bit immediates.
    Operand::Imm(i64::try_from(value).expect("constant too large for an immediate"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::SourceMap;
    use crate::{checker, lexer, lower, parser};

    fn generate_str(text: &str) -> String {
        let mut sources = SourceMap::default();
        let file = sources.add("test.jo".into(), text.into()).unwrap();
        let (tokens, diagnostics) = lexer::lex(file, text);
        assert!(diagnostics.is_empty(), "unexpected lexer diagnostics: {diagnostics:?}");
        let ast = parser::parse(&tokens, text).expect("unexpected parse error");
        let mut symbols = Interner::default();
        let (hir, diagnostics) = checker::check(&ast, file, &sources, &mut symbols);
        assert!(diagnostics.is_empty(), "unexpected check diagnostics: {diagnostics:?}");
        asm::print(&generate(&lower::lower(&hir.unwrap()), &symbols))
    }

    /// Generates code for `text` and returns just the body of `main`, after its prologue.
    fn main_body(text: &str) -> String {
        let assembly = generate_str(text);
        let body = assembly.split_once(".Lmain.bb0:\n").unwrap().1;
        body.to_string()
    }

    #[test]
    fn minimal_program() {
        let expected = "    .intel_syntax noprefix
    .text

    .globl _start
_start:
    call main
    movzx edi, al
    mov eax, 231
    syscall

main:
    push rbp
    mov rbp, rsp
    sub rsp, 16
.Lmain.bb0:
    mov byte ptr [rbp - 1], 42
    mov al, byte ptr [rbp - 1]
    mov rsp, rbp
    pop rbp
    ret
";
        assert_eq!(generate_str("fn main() -> u8 { 42 }"), expected);
    }

    #[test]
    fn copies_go_through_a_register() {
        let expected = "    mov byte ptr [rbp - 2], 7
    mov al, byte ptr [rbp - 2]
    mov byte ptr [rbp - 1], al
    mov al, byte ptr [rbp - 1]
    mov rsp, rbp
    pop rbp
    ret
";
        assert_eq!(main_body("fn main() -> u8 { let x: u8 = 7; x }"), expected);
    }

    #[test]
    fn frame_layout() {
        let local = |ty| LocalDecl { ty, name: None };
        let frame = layout_frame(&[local(Type::U8), local(Type::Unit), local(Type::U8), local(Type::U8)]);
        assert_eq!(frame.offsets, [Some(-1), None, Some(-2), Some(-3)]);
        assert_eq!(frame.size, 16);
        assert_eq!(layout_frame(&vec![local(Type::U8); 17]).size, 32);
        assert_eq!(layout_frame(&[local(Type::Unit)]).size, 0);
        assert_eq!(layout_frame(&[]).size, 0);
    }
}
