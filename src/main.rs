mod asm;
mod ast;
mod checker;
mod codegen;
mod diagnostic;
mod hir;
mod intern;
mod ir;
mod lexer;
mod link;
mod lower;
mod parser;
mod source;

use std::path::Path;
use std::process::ExitCode;

use diagnostic::Diagnostic;
use intern::Interner;
use lexer::Token;
use source::SourceMap;

const USAGE: &str = "\
usage: jolang <file.jo> -o <output>
       jolang <file.jo> --emit=<stage>

stages: tokens, ast, hir, ir, asm";

/// A pipeline stage whose output can be printed with `--emit`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Tokens,
    Ast,
    Hir,
    Ir,
    Asm,
}

enum Output {
    /// Print the output of one stage, then stop.
    Emit(Stage),
    /// Build an executable at the given path.
    Executable(String),
}

struct Options {
    input: String,
    output: Output,
}

impl Options {
    fn emits(&self, stage: Stage) -> bool {
        matches!(self.output, Output::Emit(emit) if emit == stage)
    }
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut input = None;
    let mut output = None;

    while let Some(arg) = args.next() {
        let new_output = if arg == "-o" {
            let path = args.next().ok_or("`-o` requires a path")?;
            Output::Executable(path)
        } else if let Some(stage) = arg.strip_prefix("--emit=") {
            let stage = match stage {
                "tokens" => Stage::Tokens,
                "ast" => Stage::Ast,
                "hir" => Stage::Hir,
                "ir" => Stage::Ir,
                "asm" => Stage::Asm,
                _ => return Err(format!("unknown stage `{stage}`")),
            };
            Output::Emit(stage)
        } else if arg.starts_with('-') {
            return Err(format!("unknown option `{arg}`"));
        } else {
            if input.is_some() {
                return Err("only one input file can be given".into());
            }
            input = Some(arg);
            continue;
        };

        if output.is_some() {
            return Err("only one of `-o` or `--emit` can be given".into());
        }
        output = Some(new_output);
    }

    Ok(Options {
        input: input.ok_or("no input file given")?,
        output: output.ok_or("one of `-o` or `--emit` is required")?,
    })
}

fn main() -> ExitCode {
    let options = match parse_args(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("error: {message}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };

    match compile(&options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(()) => ExitCode::FAILURE,
    }
}

/// Runs the pipeline. Errors are printed as they are found.
fn compile(options: &Options) -> Result<(), ()> {
    let text = std::fs::read_to_string(&options.input)
        .map_err(|err| eprintln!("error: could not read `{}`: {err}", options.input))?;
    let mut sources = SourceMap::default();
    let file = sources.add(options.input.clone(), text).map_err(|message| eprintln!("error: {message}"))?;

    let text = &sources.get(file).text;

    let (tokens, diagnostics) = lexer::lex(file, text);
    if !diagnostics.is_empty() {
        print_diagnostics(&diagnostics, &sources);
        return Err(());
    }
    if options.emits(Stage::Tokens) {
        print_tokens(&tokens, &sources);
        return Ok(());
    }

    let ast = parser::parse(&tokens, text).map_err(|diagnostic| print_diagnostics(&[diagnostic], &sources))?;
    if options.emits(Stage::Ast) {
        print!("{}", ast.dump(&sources));
        return Ok(());
    }

    let mut symbols = Interner::default();
    let hir = checker::check(&ast, file, &sources, &mut symbols)
        .map_err(|diagnostics| print_diagnostics(&diagnostics, &sources))?;
    if options.emits(Stage::Hir) {
        print!("{}", hir.dump(&sources, &symbols));
        return Ok(());
    }

    let ir = lower::lower(&hir);
    if options.emits(Stage::Ir) {
        print!("{}", ir.dump(&symbols));
        return Ok(());
    }

    let assembly = asm::print(&codegen::generate(&ir, &symbols));
    if options.emits(Stage::Asm) {
        print!("{assembly}");
        return Ok(());
    }

    let Output::Executable(path) = &options.output else { unreachable!("every stage is emitted above") };
    link::assemble_and_link(&assembly, Path::new(path)).map_err(|message| eprintln!("error: {message}"))
}

fn print_tokens(tokens: &[Token], sources: &SourceMap) {
    for token in tokens {
        let (line, column) = sources.get(token.span.file).line_col(token.span.start);
        let location = format!("{line}:{column}");
        let kind = format!("{:?}", token.kind);
        println!("{location:<8} {kind:<8} {:?}", sources.text(token.span));
    }
}

fn print_diagnostics(diagnostics: &[Diagnostic], sources: &SourceMap) {
    for diagnostic in diagnostics {
        eprint!("{}", diagnostic.render(sources));
    }
}
