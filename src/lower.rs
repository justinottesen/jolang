//! Lowers the HIR to the IR: turns nested expressions into a control-flow graph of statements.
//!
//! Expressions are lowered with a destination: the place their value should be stored in. See
//! "Lowering" in design/compiler.md.

use crate::ast::{BinaryOp, UnaryOp};
use crate::hir::{self, Hir, Type};
use crate::ir::{self, BinOp, Block, Ir, Local, LocalDecl, Operand, Place, Rvalue, Statement, Terminator};

/// Lowers a checked program. Lowering can't fail: the HIR is already known to be valid.
pub fn lower(hir: &Hir) -> Ir {
    let functions = hir.fns.iter().map(|function| lower_function(hir, function)).collect();
    Ir { functions }
}

fn lower_function(hir: &Hir, function: &hir::Function) -> ir::Function {
    // `_0` is the return value, followed by one IR local per HIR local, in the same order.
    let mut locals = vec![LocalDecl { ty: function.return_type, name: None }];
    locals.extend(function.locals.iter().map(|local| LocalDecl { ty: local.ty, name: Some(local.name) }));

    let mut builder = FunctionBuilder { hir, locals, blocks: Vec::new(), statements: Vec::new() };
    builder.lower_expr_into(function.body, Some(Place { local: Local::RETURN }));
    builder.finish_block(Terminator::Return);
    ir::Function { name: function.name, locals: builder.locals, blocks: builder.blocks }
}

/// Whether an expression can be used as an operand without evaluating anything first: a constant
/// or a variable.
fn is_simple(expr: &hir::Expr) -> bool {
    matches!(expr.kind, hir::ExprKind::Integer { .. } | hir::ExprKind::Local(_))
}

struct FunctionBuilder<'a> {
    hir: &'a Hir,
    locals: Vec<LocalDecl>,
    /// Blocks that have been finished.
    blocks: Vec<Block>,
    /// Statements of the block currently being built.
    statements: Vec<Statement>,
}

impl FunctionBuilder<'_> {
    /// Ends the current block with `terminator`, and starts a new one.
    fn finish_block(&mut self, terminator: Terminator) {
        let statements = std::mem::take(&mut self.statements);
        self.blocks.push(Block { statements, terminator });
    }

    fn local_place(&self, local: hir::LocalId) -> Place {
        Place { local: Local(local.0 + 1) }
    }

    fn new_temp(&mut self, ty: Type) -> Place {
        let local = Local(self.locals.len() as u32);
        self.locals.push(LocalDecl { ty, name: None });
        Place { local }
    }

    /// Stores `value` in `place`. Storing a `()` does nothing, since it has no data.
    fn assign(&mut self, place: Place, value: Rvalue) {
        if self.locals[place.local.0 as usize].ty != Type::Unit {
            self.statements.push(Statement::Assign { place, value });
        }
    }

    /// Emits statements that evaluate `id` and store its value in `destination`. With no
    /// destination, the value is discarded; the checker only allows that for `()`.
    fn lower_expr_into(&mut self, id: hir::ExprId, destination: Option<Place>) {
        let expr = *self.hir.expr(id);
        match expr.kind {
            hir::ExprKind::Integer { .. } | hir::ExprKind::Local(_) => {
                if let Some(destination) = destination {
                    let operand = self.simple_operand(&expr);
                    self.assign(destination, Rvalue::Use(operand));
                }
            }
            hir::ExprKind::Unit => {}
            hir::ExprKind::Block { stmts, tail } => {
                for stmt in self.hir.stmts(stmts) {
                    self.lower_stmt(*stmt);
                }
                if let Some(tail) = tail {
                    self.lower_expr_into(tail, destination);
                }
            }
            hir::ExprKind::Assign { target, value } => {
                let target = self.lower_place(target);
                // A simple value can be stored directly. Anything else is evaluated into a
                // temporary first, so the target never holds a partly computed value while the
                // value is still being evaluated (which may read the target).
                let value_expr = *self.hir.expr(value);
                if is_simple(&value_expr) {
                    self.lower_expr_into(value, Some(target));
                } else {
                    let temp = self.new_temp(value_expr.ty);
                    self.lower_expr_into(value, Some(temp));
                    self.assign(target, Rvalue::Use(Operand::Copy(temp)));
                }
                // The assignment's own value is `()`, so there is nothing to store in `destination`.
            }
            hir::ExprKind::Unary { op, operand } => {
                let operand = self.lower_operand(operand);
                let rvalue = Rvalue::Neg { checked: op == UnaryOp::Neg, operand };
                self.compute_into(destination, expr.ty, rvalue);
            }
            hir::ExprKind::Binary { op, lhs, rhs } => {
                // The left operand is evaluated first. If evaluating the right one runs statements,
                // they might change a variable used on the left, so the left side is then copied to
                // a temporary even if it's simple.
                let lhs = if is_simple(self.hir.expr(rhs)) {
                    self.lower_operand(lhs)
                } else {
                    self.lower_operand_into_temp(lhs)
                };
                let rhs = self.lower_operand(rhs);
                let (op, checked) = match op {
                    BinaryOp::Add => (BinOp::Add, true),
                    BinaryOp::Sub => (BinOp::Sub, true),
                    BinaryOp::Mul => (BinOp::Mul, true),
                    BinaryOp::Div => (BinOp::Div, true),
                    BinaryOp::Rem => (BinOp::Rem, true),
                    BinaryOp::WrappingAdd => (BinOp::Add, false),
                    BinaryOp::WrappingSub => (BinOp::Sub, false),
                    BinaryOp::WrappingMul => (BinOp::Mul, false),
                };
                self.compute_into(destination, expr.ty, Rvalue::Binary { op, checked, lhs, rhs });
            }
            hir::ExprKind::Cast { operand, wrapping } => {
                let (Type::Int(from), Type::Int(to)) = (self.hir.expr(operand).ty, expr.ty) else {
                    unreachable!("the checker only allows converting integers");
                };
                let operand = self.lower_operand(operand);
                // A conversion that always fits can't fail, so it needs no check.
                let checked = !wrapping && !to.contains(from);
                self.compute_into(destination, expr.ty, Rvalue::Cast { checked, operand });
            }
        }
    }

    /// Stores the result of an rvalue in `destination`. The rvalue may trap, so it's evaluated
    /// even when its value is discarded.
    fn compute_into(&mut self, destination: Option<Place>, ty: Type, rvalue: Rvalue) {
        let destination = destination.unwrap_or_else(|| self.new_temp(ty));
        self.assign(destination, rvalue);
    }

    /// Lowers an expression for use as an operand. Simple expressions are used directly; anything
    /// else is evaluated into a temporary.
    fn lower_operand(&mut self, id: hir::ExprId) -> Operand {
        let expr = *self.hir.expr(id);
        if is_simple(&expr) { self.simple_operand(&expr) } else { self.lower_operand_into_temp(id) }
    }

    /// Lowers an expression into a new temporary, and returns it as an operand.
    fn lower_operand_into_temp(&mut self, id: hir::ExprId) -> Operand {
        let temp = self.new_temp(self.hir.expr(id).ty);
        self.lower_expr_into(id, Some(temp));
        Operand::Copy(temp)
    }

    fn simple_operand(&self, expr: &hir::Expr) -> Operand {
        match expr.kind {
            hir::ExprKind::Integer { value } => Operand::Constant { value, ty: expr.ty },
            hir::ExprKind::Local(local) => Operand::Copy(self.local_place(local)),
            _ => unreachable!("not a simple expression"),
        }
    }

    fn lower_stmt(&mut self, stmt: hir::Stmt) {
        match stmt {
            hir::Stmt::Let { local, init } => {
                let place = self.local_place(local);
                self.lower_expr_into(init, Some(place));
            }
            hir::Stmt::Expr(expr) => self.lower_expr_into(expr, None),
        }
    }

    /// Returns the place an expression refers to, for the target of an assignment.
    fn lower_place(&self, id: hir::ExprId) -> Place {
        match self.hir.expr(id).kind {
            hir::ExprKind::Local(local) => self.local_place(local),
            _ => unreachable!("the checker only allows assigning to variables"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intern::Interner;
    use crate::source::SourceMap;
    use crate::{checker, lexer, parser};

    fn lower_str(text: &str) -> String {
        let mut sources = SourceMap::default();
        let file = sources.add("test.jo".into(), text.into()).unwrap();
        let (tokens, diagnostics) = lexer::lex(file, text);
        assert!(diagnostics.is_empty(), "unexpected lexer diagnostics: {diagnostics:?}");
        let ast = parser::parse(&tokens, text).expect("unexpected parse error");
        let mut symbols = Interner::default();
        let (hir, diagnostics) = checker::check(&ast, file, &sources, &mut symbols);
        assert!(diagnostics.is_empty(), "unexpected check diagnostics: {diagnostics:?}");
        lower(&hir.unwrap()).dump(&symbols)
    }

    /// Lowers `body` as the body of `main`, and returns just its statements.
    fn statements(body: &str) -> String {
        let ir = lower_str(&format!("fn main() -> u8 {{ {body} }}"));
        let lines: Vec<&str> = ir.lines().skip_while(|line| *line != "  bb0:").skip(1).collect();
        lines[..lines.len() - 2].iter().map(|line| line.trim()).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn minimal_program() {
        let expected = "\
fn main() -> u8 {
    let _0: u8
  bb0:
    _0 = const 42_u8
    return
}
";
        assert_eq!(lower_str("fn main() -> u8 { 42 }"), expected);
    }

    #[test]
    fn variables() {
        let expected = "\
fn main() -> u8 {
    let _0: u8
    let _1: u8  // x
    let _2: u8  // y
  bb0:
    _1 = const 1_u8
    _2 = copy _1
    _2 = const 2_u8
    _0 = copy _2
    return
}
";
        assert_eq!(lower_str("fn main() -> u8 { let x: u8 = 1; let mut y = x; y = 2; y }"), expected);
    }

    #[test]
    fn assigning_a_block_uses_a_temporary() {
        let expected = "\
_1 = const 1_u8
_2 = copy _1
_1 = copy _2
_0 = copy _1";
        assert_eq!(statements("let mut x: u8 = 1; x = { x }; x"), expected);
    }

    #[test]
    fn unit_values_store_nothing() {
        let expected = "\
_1 = const 1_u8
_1 = const 2_u8
_0 = copy _1";
        assert_eq!(statements("let mut x: u8 = 1; let unit: () = x = 2; unit; x"), expected);
    }

    #[test]
    fn arithmetic() {
        // Nested operations go through temporaries; the outermost writes straight to `_0`.
        let expected = "\
_1 = const 2_u8
_2 = checked_mul(copy _1, const 3_u8)
_0 = checked_add(copy _2, const 1_u8)";
        assert_eq!(statements("let x: u8 = 2; x * 3 + 1"), expected);
        assert_eq!(statements("let x: u8 = 2; x +% 1"), "_1 = const 2_u8\n_0 = wrapping_add(copy _1, const 1_u8)");
        assert_eq!(statements("let x: u8 = 2; x / 1"), "_1 = const 2_u8\n_0 = checked_div(copy _1, const 1_u8)");
    }

    #[test]
    fn left_operand_is_saved_before_right_side_runs() {
        // The right side assigns to `x`, so the left `x` is copied first to keep its old value.
        let expected = "\
_1 = const 1_u8
_2 = copy _1
_1 = const 5_u8
_3 = copy _1
_0 = checked_add(copy _2, copy _3)";
        assert_eq!(statements("let mut x: u8 = 1; x + { x = 5; x }"), expected);
    }

    #[test]
    fn negation() {
        let expected = "\
_1 = const -5_i8
_2 = checked_neg(copy _1)
_0 = checked_cast(copy _2) to u8";
        assert_eq!(statements("let x: i8 = -5; (-x) as u8"), expected);
    }

    #[test]
    fn conversions_that_always_fit_are_unchecked() {
        assert_eq!(
            statements("let x: u8 = 1; let y: u32 = x as u32; y as u8"),
            "_1 = const 1_u8\n_2 = wrapping_cast(copy _1) to u32\n_0 = checked_cast(copy _2) to u8"
        );
        assert_eq!(statements("let x: u32 = 1; x as% u8"), "_1 = const 1_u32\n_0 = wrapping_cast(copy _1) to u8");
    }
}
