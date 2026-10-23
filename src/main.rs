mod diagnostic;
mod lexer;
mod source;

use std::process::ExitCode;

use diagnostic::Diagnostic;
use source::SourceMap;

const USAGE: &str = "\
usage: jolang <file.jo> -o <output>
       jolang <file.jo> --emit=<stage>

stages: tokens";

/// A pipeline stage whose output can be printed with `--emit`.
enum Stage {
    Tokens,
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

    let (tokens, diagnostics) = lexer::lex(file, &sources.get(file).text);
    report(&diagnostics, &sources)?;
    match &options.output {
        Output::Emit(Stage::Tokens) => {
            for token in &tokens {
                let (line, column) = sources.get(file).line_col(token.span.start);
                let location = format!("{line}:{column}");
                let kind = format!("{:?}", token.kind);
                println!("{location:<8} {kind:<8} {:?}", sources.text(token.span));
            }
            Ok(())
        }
        Output::Executable(path) => {
            eprintln!("error: cannot build `{path}`: building executables is not implemented yet");
            Err(())
        }
    }
}

/// Prints diagnostics. Fails if there were any, since every diagnostic is currently an error.
fn report(diagnostics: &[Diagnostic], sources: &SourceMap) -> Result<(), ()> {
    for diagnostic in diagnostics {
        eprint!("{}", diagnostic.render(sources));
    }
    if diagnostics.is_empty() { Ok(()) } else { Err(()) }
}
