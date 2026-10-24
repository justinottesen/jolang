//! Generates x86-64 instructions from the IR, for Linux.
//!
//! Every local gets its own slot in the stack frame. Each statement loads its operands from their
//! slots into registers, computes, and stores the result back. This is simple, and makes slow
//! code. See "Codegen" in design/compiler.md.

use crate::asm::{self, Cond, Inst, Operand, Reg, Size};
use crate::hir::{IntType, Type};
use crate::intern::Interner;
use crate::ir::{
    self, BinOp, BlockId, CmpOp, Ir, Local, LocalDecl, Place, Rvalue, Statement, Terminator,
};

/// Generates the whole program: an entry point, plus every function in the IR.
pub fn generate(ir: &Ir, symbols: &Interner) -> Vec<asm::Function> {
    let mut functions = vec![linux_entry_point()];
    functions.extend(
        ir.functions
            .iter()
            .map(|function| generate_function(function, symbols)),
    );
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
        Inst::Movzx {
            dst: Operand::Reg(Reg::Rdi, Size::Dword),
            src: Operand::Reg(Reg::Rax, Size::Byte),
        },
        Inst::Mov {
            dst: Operand::Reg(Reg::Rax, Size::Dword),
            src: Operand::Imm(SYS_EXIT_GROUP),
        },
        Inst::Syscall,
    ];
    asm::Function {
        name: "_start".into(),
        global: true,
        insts,
    }
}

/// The size of a value of type `ty` in memory. `()` has no size, since it has no data, so no
/// instruction ever reads or writes one.
fn value_size(ty: Type) -> Option<Size> {
    match ty {
        Type::Unit => None,
        Type::Bool => Some(Size::Byte),
        Type::Int(int) => Some(int_size(int)),
    }
}

fn int_size(int: IntType) -> Size {
    match int.bits {
        8 => Size::Byte,
        16 => Size::Word,
        32 => Size::Dword,
        64 => Size::Qword,
        bits => unreachable!("unsupported integer width {bits}"),
    }
}

/// The register width that arithmetic on `int` is done at. 8- and 16-bit values are widened to 32
/// bits, where every operation on them gives the exact result, so overflow is checked by comparing
/// that result against the type's range. 32- and 64-bit values use the CPU's overflow flags.
fn work_size(int: IntType) -> Size {
    if int.bits <= 32 {
        Size::Dword
    } else {
        Size::Qword
    }
}

fn is_narrow(int: IntType) -> bool {
    int.bits < 32
}

/// The integer type a value is handled as in registers. A `bool` is an unsigned byte holding 0 or 1.
fn int_type(ty: Type) -> IntType {
    match ty {
        Type::Int(int) => int,
        Type::Bool => IntType {
            signed: false,
            bits: 8,
        },
        Type::Unit => unreachable!("no instruction reads or writes `()`"),
    }
}

fn imm(value: i128) -> Operand {
    // Immediates are written as the value's bits as a signed 64-bit number; a `u64` above
    // `i64::MAX` comes out negative, but its bits are the same.
    Operand::Imm(value as i64)
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
        let Some(size) = value_size(local.ty) else {
            offsets.push(None);
            continue;
        };
        // Slots grow downward from `rbp`. Every type's alignment is currently its size.
        let bytes = size.bytes();
        used = (used + bytes).next_multiple_of(bytes);
        offsets.push(Some(-(used as i32)));
    }
    Frame {
        offsets,
        size: used.next_multiple_of(16),
    }
}

fn generate_function(function: &ir::Function, symbols: &Interner) -> asm::Function {
    let name = symbols.get(function.name);
    let mut generator = FunctionGenerator {
        function,
        frame: layout_frame(&function.locals),
        name,
        insts: Vec::new(),
        uses_trap: false,
        labels: 0,
    };
    generator.generate();
    asm::Function {
        name: name.to_string(),
        global: false,
        insts: generator.insts,
    }
}

struct FunctionGenerator<'a> {
    function: &'a ir::Function,
    frame: Frame,
    name: &'a str,
    insts: Vec<Inst>,
    /// Whether anything jumps to the trap label, so it needs to be emitted.
    uses_trap: bool,
    /// How many local labels have been created.
    labels: u32,
}

impl FunctionGenerator<'_> {
    fn generate(&mut self) {
        // Prologue: save the caller's frame pointer, and reserve space for this frame.
        self.push(Inst::Push(Reg::Rbp));
        self.push(Inst::Mov {
            dst: reg(Reg::Rbp, Size::Qword),
            src: reg(Reg::Rsp, Size::Qword),
        });
        if self.frame.size > 0 {
            self.push(Inst::Sub {
                dst: reg(Reg::Rsp, Size::Qword),
                src: Operand::Imm(self.frame.size.into()),
            });
        }

        for (i, block) in self.function.blocks.iter().enumerate() {
            self.push(Inst::Label(self.block_label(BlockId(i as u32))));
            for statement in &block.statements {
                match *statement {
                    Statement::Assign { place, value } => self.assign(place, value),
                }
            }
            // Blocks are laid out in order, so jumping to the next block isn't needed: execution
            // falls through to it.
            let next = BlockId(i as u32 + 1);
            match block.terminator {
                Terminator::Return => self.ret(),
                Terminator::Goto(target) => {
                    if target != next {
                        self.push(Inst::Jmp(self.block_label(target)));
                    }
                }
                Terminator::Branch {
                    cond,
                    then_block,
                    else_block,
                } => {
                    let al = reg(Reg::Rax, Size::Byte);
                    self.load(Reg::Rax, cond, Size::Byte);
                    self.push(Inst::Test { lhs: al, rhs: al });
                    if then_block == next {
                        self.push(Inst::Jcc(Cond::Equal, self.block_label(else_block)));
                    } else {
                        self.push(Inst::Jcc(Cond::NotEqual, self.block_label(then_block)));
                        if else_block != next {
                            self.push(Inst::Jmp(self.block_label(else_block)));
                        }
                    }
                }
            }
        }

        // Every failed check in the function jumps here.
        if self.uses_trap {
            self.push(Inst::Label(self.trap_label()));
            self.push(Inst::Ud2);
        }
    }

    fn push(&mut self, inst: Inst) {
        self.insts.push(inst);
    }

    fn block_label(&self, block: BlockId) -> String {
        format!(".L{}.bb{}", self.name, block.0)
    }

    fn trap_label(&self) -> String {
        format!(".L{}.trap", self.name)
    }

    fn new_label(&mut self) -> String {
        self.labels += 1;
        format!(".L{}.{}", self.name, self.labels)
    }

    fn trap_if(&mut self, cond: Cond) {
        self.uses_trap = true;
        self.push(Inst::Jcc(cond, self.trap_label()));
    }

    fn place_type(&self, place: Place) -> Type {
        self.function.locals[place.local.0 as usize].ty
    }

    fn operand_type(&self, operand: ir::Operand) -> Type {
        match operand {
            ir::Operand::Constant { ty, .. } => ty,
            ir::Operand::Copy(place) => self.place_type(place),
        }
    }

    /// The memory operand for a place's slot. Lowering never reads or writes a place of type `()`.
    fn slot(&self, place: Place) -> Operand {
        let offset =
            self.frame.offsets[place.local.0 as usize].expect("access to a zero-sized local");
        let size = value_size(self.place_type(place)).expect("access to a zero-sized local");
        Operand::Mem {
            base: Reg::Rbp,
            offset,
            size,
        }
    }

    /// Loads an operand into `reg` at `width`, extending it according to its signedness, so the
    /// register holds exactly the operand's value.
    fn load(&mut self, into: Reg, operand: ir::Operand, width: Size) {
        let dst = reg(into, width);
        let place = match operand {
            ir::Operand::Constant { value, .. } => {
                return self.push(Inst::Mov {
                    dst,
                    src: imm(value),
                });
            }
            ir::Operand::Copy(place) => place,
        };
        let int = int_type(self.place_type(place));
        let src = self.slot(place);
        let inst = if int_size(int) == width {
            Inst::Mov { dst, src }
        } else if int.signed {
            Inst::Movsx { dst, src }
        } else if int_size(int) == Size::Dword {
            // Writing a 32-bit register clears the upper half, which zero-extends the value.
            Inst::Mov {
                dst: reg(into, Size::Dword),
                src,
            }
        } else {
            Inst::Movzx { dst, src }
        };
        self.push(inst);
    }

    /// Stores the low bits of `from` into a place's slot.
    fn store(&mut self, place: Place, from: Reg) {
        let dst = self.slot(place);
        let size = value_size(self.place_type(place)).unwrap();
        self.push(Inst::Mov {
            dst,
            src: reg(from, size),
        });
    }

    fn assign(&mut self, place: Place, value: Rvalue) {
        let ty = int_type(self.place_type(place));
        match value {
            // x86 can't move directly from memory to memory, so go through `rax`.
            Rvalue::Use(operand) => self.load(Reg::Rax, operand, int_size(ty)),
            Rvalue::Binary {
                op,
                checked,
                lhs,
                rhs,
            } => self.binary(ty, op, checked, lhs, rhs),
            Rvalue::Neg { checked, operand } => {
                let width = work_size(ty);
                self.load(Reg::Rax, operand, width);
                self.push(Inst::Neg(reg(Reg::Rax, width)));
                if checked {
                    self.check_overflow(ty, Cond::Overflow);
                }
            }
            Rvalue::Cast { checked, operand } => self.cast(ty, checked, operand),
            Rvalue::Compare { op, lhs, rhs } => self.compare(op, lhs, rhs),
            Rvalue::Not(operand) => {
                // A `bool` is 0 or 1, so flipping the lowest bit negates it.
                let al = reg(Reg::Rax, Size::Byte);
                self.load(Reg::Rax, operand, Size::Byte);
                self.push(Inst::Xor {
                    dst: al,
                    src: Operand::Imm(1),
                });
            }
        }
        self.store(place, Reg::Rax);
    }

    /// Compares two operands, leaving the `bool` result in `al`.
    fn compare(&mut self, op: CmpOp, lhs: ir::Operand, rhs: ir::Operand) {
        let ty = int_type(self.operand_type(lhs));
        let width = work_size(ty);
        self.load(Reg::Rax, lhs, width);
        self.load(Reg::Rcx, rhs, width);
        self.push(Inst::Cmp {
            lhs: reg(Reg::Rax, width),
            rhs: reg(Reg::Rcx, width),
        });
        let cond = match (op, ty.signed) {
            (CmpOp::Eq, _) => Cond::Equal,
            (CmpOp::Ne, _) => Cond::NotEqual,
            (CmpOp::Lt, true) => Cond::Less,
            (CmpOp::Le, true) => Cond::LessEqual,
            (CmpOp::Gt, true) => Cond::Greater,
            (CmpOp::Ge, true) => Cond::GreaterEqual,
            (CmpOp::Lt, false) => Cond::Below,
            (CmpOp::Le, false) => Cond::BelowEqual,
            (CmpOp::Gt, false) => Cond::Above,
            (CmpOp::Ge, false) => Cond::AboveEqual,
        };
        self.push(Inst::Setcc(cond, reg(Reg::Rax, Size::Byte)));
    }

    /// Computes a binary operation on values of type `ty`, leaving the result in `rax`.
    fn binary(
        &mut self,
        ty: IntType,
        op: BinOp,
        checked: bool,
        lhs: ir::Operand,
        rhs: ir::Operand,
    ) {
        let width = work_size(ty);
        let (rax, rcx) = (reg(Reg::Rax, width), reg(Reg::Rcx, width));
        self.load(Reg::Rax, lhs, width);
        self.load(Reg::Rcx, rhs, width);
        // The flag that signals overflow at the work width, for types that use flags.
        let overflow_flag = if ty.signed {
            Cond::Overflow
        } else {
            Cond::Carry
        };
        match op {
            BinOp::Add => {
                self.push(Inst::Add { dst: rax, src: rcx });
                if checked {
                    self.check_overflow(ty, overflow_flag);
                }
            }
            BinOp::Sub => {
                self.push(Inst::Sub { dst: rax, src: rcx });
                if checked {
                    self.check_overflow(ty, overflow_flag);
                }
            }
            // `imul` only reports signed overflow, so checked unsigned multiplication at the work
            // width needs `mul`, which sets the carry flag if the product doesn't fit.
            BinOp::Mul if checked && !ty.signed && !is_narrow(ty) => {
                self.push(Inst::Mul(rcx));
                self.trap_if(Cond::Carry);
            }
            BinOp::Mul => {
                self.push(Inst::Imul { dst: rax, src: rcx });
                if checked {
                    self.check_overflow(ty, Cond::Overflow);
                }
            }
            BinOp::Div | BinOp::Rem => self.divide(ty, op),
        }
    }

    /// Divides `rax` by `rcx`, both of type `ty` at its work width. Leaves the quotient or
    /// remainder in `rax`.
    fn divide(&mut self, ty: IntType, op: BinOp) {
        let width = work_size(ty);
        let (rax, rcx, rdx) = (
            reg(Reg::Rax, width),
            reg(Reg::Rcx, width),
            reg(Reg::Rdx, width),
        );

        // Division by zero traps.
        self.push(Inst::Test { lhs: rcx, rhs: rcx });
        self.trap_if(Cond::Equal);

        if ty.signed {
            // `MIN / -1` overflows, since the result would be `MAX + 1`. `MIN % -1` traps too, so
            // the two always behave the same.
            let not_minus_one = self.new_label();
            self.push(Inst::Cmp {
                lhs: rcx,
                rhs: Operand::Imm(-1),
            });
            self.push(Inst::Jcc(Cond::NotEqual, not_minus_one.clone()));
            self.push(Inst::Mov {
                dst: rdx,
                src: imm(ty.min()),
            });
            self.push(Inst::Cmp { lhs: rax, rhs: rdx });
            self.trap_if(Cond::Equal);
            self.push(Inst::Label(not_minus_one));
            // `idiv` divides `rdx:rax`, so sign-extend `rax` into `rdx`.
            self.push(if width == Size::Dword {
                Inst::Cdq
            } else {
                Inst::Cqo
            });
            self.push(Inst::Idiv(rcx));
        } else {
            // `div` divides `rdx:rax`, so clear `rdx`. Clearing `edx` also clears the upper half.
            let edx = reg(Reg::Rdx, Size::Dword);
            self.push(Inst::Xor { dst: edx, src: edx });
            self.push(Inst::Div(rcx));
        }

        if op == BinOp::Rem {
            self.push(Inst::Mov { dst: rax, src: rdx });
        }
    }

    /// After an operation at the work width, traps if the result in `rax` doesn't fit in `ty`.
    /// For narrow types the result is exact, so it's compared against the range. For wider types,
    /// the operation set `flag` on overflow.
    fn check_overflow(&mut self, ty: IntType, flag: Cond) {
        if !is_narrow(ty) {
            return self.trap_if(flag);
        }
        let eax = reg(Reg::Rax, Size::Dword);
        self.push(Inst::Cmp {
            lhs: eax,
            rhs: imm(ty.max()),
        });
        if ty.signed {
            self.trap_if(Cond::Greater);
            self.push(Inst::Cmp {
                lhs: eax,
                rhs: imm(ty.min()),
            });
            self.trap_if(Cond::Less);
        } else {
            // A negative result looks like a huge unsigned number, so this catches it too.
            self.trap_if(Cond::Above);
        }
    }

    /// Converts an operand to type `to`, leaving the result in `rax`.
    fn cast(&mut self, to: IntType, checked: bool, operand: ir::Operand) {
        let from = int_type(self.operand_type(operand));
        // Load at 64 bits, so `rax` holds the operand's exact value. Storing the result then keeps
        // the low bits.
        self.load(Reg::Rax, operand, Size::Qword);
        if !checked {
            return;
        }

        let (rax, rcx) = (reg(Reg::Rax, Size::Qword), reg(Reg::Rcx, Size::Qword));
        // Bounds go through `rcx`, since `cmp` can only take a 32-bit immediate.
        if from.signed {
            if to.min() > from.min() {
                if to.min() == 0 {
                    self.push(Inst::Test { lhs: rax, rhs: rax });
                    self.trap_if(Cond::Sign);
                } else {
                    self.push(Inst::Mov {
                        dst: rcx,
                        src: imm(to.min()),
                    });
                    self.push(Inst::Cmp { lhs: rax, rhs: rcx });
                    self.trap_if(Cond::Less);
                }
            }
            if to.max() < from.max() {
                self.push(Inst::Mov {
                    dst: rcx,
                    src: imm(to.max()),
                });
                self.push(Inst::Cmp { lhs: rax, rhs: rcx });
                self.trap_if(Cond::Greater);
            }
        } else if to.max() < from.max() {
            // The value is unsigned, so compare unsigned.
            self.push(Inst::Mov {
                dst: rcx,
                src: imm(to.max()),
            });
            self.push(Inst::Cmp { lhs: rax, rhs: rcx });
            self.trap_if(Cond::Above);
        }
    }

    fn ret(&mut self) {
        let return_type = self.place_type(Place {
            local: Local::RETURN,
        });
        if let Some(size) = value_size(return_type) {
            let src = self.slot(Place {
                local: Local::RETURN,
            });
            self.push(Inst::Mov {
                dst: reg(Reg::Rax, size),
                src,
            });
        }
        // Epilogue: free the frame, and restore the caller's frame pointer.
        self.push(Inst::Mov {
            dst: reg(Reg::Rsp, Size::Qword),
            src: reg(Reg::Rbp, Size::Qword),
        });
        self.push(Inst::Pop(Reg::Rbp));
        self.push(Inst::Ret);
    }
}

fn reg(reg: Reg, size: Size) -> Operand {
    Operand::Reg(reg, size)
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
        assert!(
            diagnostics.is_empty(),
            "unexpected lexer diagnostics: {diagnostics:?}"
        );
        let ast = parser::parse(&tokens, text).expect("unexpected parse error");
        let mut symbols = Interner::default();
        let (hir, diagnostics) = checker::check(&ast, file, &sources, &mut symbols);
        assert!(
            diagnostics.is_empty(),
            "unexpected check diagnostics: {diagnostics:?}"
        );
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
    mov al, 42
    mov byte ptr [rbp - 1], al
    mov al, byte ptr [rbp - 1]
    mov rsp, rbp
    pop rbp
    ret
";
        assert_eq!(generate_str("fn main() -> u8 { 42 }"), expected);
    }

    #[test]
    fn copies_go_through_a_register() {
        let expected = "    mov al, 7
    mov byte ptr [rbp - 2], al
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
    fn narrow_checked_add_compares_against_the_range() {
        let body = main_body("fn main() -> u8 { let x: u8 = 7; x + 1 }");
        assert!(
            body.contains(
                "    movzx eax, byte ptr [rbp - 2]
    mov ecx, 1
    add eax, ecx
    cmp eax, 255
    ja .Lmain.trap
    mov byte ptr [rbp - 1], al
"
            ),
            "{body}"
        );
        assert!(body.ends_with(".Lmain.trap:\n    ud2\n"), "{body}");
    }

    #[test]
    fn wide_checked_add_uses_flags() {
        let unsigned = main_body("fn main() -> u8 { let x: u64 = 7; (x + 1) as% u8 }");
        assert!(
            unsigned.contains("add rax, rcx\n    jc .Lmain.trap\n"),
            "{unsigned}"
        );
        let signed = main_body("fn main() -> u8 { let x: i32 = 7; (x + 1) as% u8 }");
        assert!(
            signed.contains("add eax, ecx\n    jo .Lmain.trap\n"),
            "{signed}"
        );
    }

    #[test]
    fn wrapping_add_has_no_check() {
        let body = main_body("fn main() -> u8 { let x: u8 = 7; x +% 1 }");
        assert!(!body.contains("trap"), "{body}");
    }

    #[test]
    fn frame_layout() {
        let local = |ty| LocalDecl { ty, name: None };
        let i32 = Type::Int(IntType {
            signed: true,
            bits: 32,
        });
        let frame = layout_frame(&[
            local(Type::U8),
            local(Type::Unit),
            local(i32),
            local(Type::U8),
        ]);
        assert_eq!(frame.offsets, [Some(-1), None, Some(-8), Some(-9)]);
        assert_eq!(frame.size, 16);
        assert_eq!(layout_frame(&vec![local(Type::U8); 17]).size, 32);
        assert_eq!(layout_frame(&[local(Type::Unit)]).size, 0);
        assert_eq!(layout_frame(&[]).size, 0);
    }
}
