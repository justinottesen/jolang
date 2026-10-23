//! Lowers the HIR to the IR: turns nested expressions into a control-flow graph of statements.
//!
//! Expressions are lowered with a destination: the place their value should be stored in. See
//! "Lowering" in design/compiler.md.

use crate::hir::{self, Hir, Type};
use crate::ir::{self, Block, Ir, Local, LocalDecl, Operand, Place, Statement, Terminator};

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
    fn assign(&mut self, place: Place, value: Operand) {
        if self.locals[place.local.0 as usize].ty != Type::Unit {
            self.statements.push(Statement::Assign { place, value });
        }
    }

    /// Emits statements that evaluate `id` and store its value in `destination`. With no
    /// destination, the value is discarded; the checker only allows that for `()`.
    fn lower_expr_into(&mut self, id: hir::ExprId, destination: Option<Place>) {
        let expr = *self.hir.expr(id);
        match expr.kind {
            hir::ExprKind::Integer { value } => {
                if let Some(destination) = destination {
                    self.assign(destination, Operand::Constant { value, ty: expr.ty });
                }
            }
            hir::ExprKind::Local(local) => {
                if let Some(destination) = destination {
                    let source = self.local_place(local);
                    self.assign(destination, Operand::Copy(source));
                }
            }
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
                let value_expr = self.hir.expr(value);
                match value_expr.kind {
                    hir::ExprKind::Integer { .. } | hir::ExprKind::Local(_) => self.lower_expr_into(value, Some(target)),
                    _ => {
                        let temp = self.new_temp(value_expr.ty);
                        self.lower_expr_into(value, Some(temp));
                        self.assign(target, Operand::Copy(temp));
                    }
                }
                // The assignment's own value is `()`, so there is nothing to store in `destination`.
            }
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
fn main() -> u8 {
    let _0: u8
    let _1: u8  // x
    let _2: u8
  bb0:
    _1 = const 1_u8
    _2 = copy _1
    _1 = copy _2
    _0 = copy _1
    return
}
";
        assert_eq!(lower_str("fn main() -> u8 { let mut x: u8 = 1; x = { x }; x }"), expected);
    }

    #[test]
    fn unit_values_store_nothing() {
        let expected = "\
fn main() -> u8 {
    let _0: u8
    let _1: u8  // x
    let _2: ()  // unit
  bb0:
    _1 = const 1_u8
    _1 = const 2_u8
    _0 = copy _1
    return
}
";
        assert_eq!(lower_str("fn main() -> u8 { let mut x: u8 = 1; let unit: () = x = 2; unit; x }"), expected);
    }
}
