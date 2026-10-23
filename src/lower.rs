//! Lowers the HIR to the IR: turns nested expressions into a control-flow graph of statements.
//!
//! Expressions are lowered with a destination: the place their value should be stored in. See
//! "Lowering" in design/compiler.md.

use crate::hir::{self, Hir};
use crate::ir::{self, Block, Ir, Local, LocalDecl, Operand, Place, Statement, Terminator};

/// Lowers a checked program. Lowering can't fail: the HIR is already known to be valid.
pub fn lower(hir: &Hir) -> Ir {
    let functions = hir.fns.iter().map(|function| lower_function(hir, function)).collect();
    Ir { functions }
}

fn lower_function(hir: &Hir, function: &hir::Function) -> ir::Function {
    let mut builder = FunctionBuilder {
        hir,
        locals: vec![LocalDecl { ty: function.return_type }],
        blocks: Vec::new(),
        statements: Vec::new(),
    };
    builder.lower_expr_into(function.body, Place { local: Local::RETURN });
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

    /// Emits statements that evaluate `id` and store its value in `destination`.
    fn lower_expr_into(&mut self, id: hir::ExprId, destination: Place) {
        let expr = self.hir.expr(id);
        match expr.kind {
            hir::ExprKind::Integer { value } => {
                let value = Operand::Constant { value, ty: expr.ty };
                self.statements.push(Statement::Assign { place: destination, value });
            }
            hir::ExprKind::Block { tail } => self.lower_expr_into(tail, destination),
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
        let hir = checker::check(&ast, file, &sources, &mut symbols).expect("unexpected check errors");
        lower(&hir).dump(&symbols)
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
}
