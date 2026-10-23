//! Compiles every Jolang program in `tests/corpus` and checks the result against the directives in
//! the program's comments. The directive format is described in `design/compiler.md`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// Until the compiler can build executables, programs are only run through the pipeline up to this
/// stage. `expect-exit` programs pass if they get there without errors.
const LAST_STAGE: &str = "tokens";

const COMPILER: &str = env!("CARGO_BIN_EXE_jolang");

fn main() -> ExitCode {
    // Like the standard test harness, any non-flag arguments are filters on the test name.
    let filters: Vec<String> = std::env::args().skip(1).filter(|arg| !arg.starts_with('-')).collect();

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");
    let mut files = Vec::new();
    find_programs(&root, &mut files);
    files.sort();

    let mut passed = 0;
    let mut failures = Vec::new();
    for path in &files {
        let name = path.strip_prefix(&root).unwrap().display().to_string();
        if !filters.is_empty() && !filters.iter().any(|filter| name.contains(filter.as_str())) {
            continue;
        }
        match check(path) {
            Ok(()) => {
                println!("ok      {name}");
                passed += 1;
            }
            Err(reason) => {
                println!("FAILED  {name}");
                failures.push((name, reason));
            }
        }
    }

    for (name, reason) in &failures {
        println!("\n---- {name} ----\n{reason}");
    }
    println!("\ncorpus: {passed} passed, {} failed", failures.len());

    if files.is_empty() {
        println!("no programs found in {}", root.display());
        return ExitCode::FAILURE;
    }
    if failures.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

fn find_programs(dir: &Path, files: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir).unwrap_or_else(|err| panic!("cannot read {}: {err}", dir.display()));
    for entry in entries {
        let path = entry.unwrap().path();
        if path.is_dir() {
            find_programs(&path, files);
        } else if path.extension().is_some_and(|ext| ext == "jo") {
            files.push(path);
        }
    }
}

/// An error, either expected by a directive or reported by the compiler.
#[derive(Debug)]
struct Error {
    /// 1-based line number. `None` for compiler errors that aren't about a location in the file.
    line: Option<usize>,
    message: String,
}

struct Directives {
    exit: Option<u8>,
    errors: Vec<Error>,
}

fn parse_directives(source: &str) -> Result<Directives, String> {
    let mut directives = Directives { exit: None, errors: Vec::new() };
    for (i, line) in source.lines().enumerate() {
        let Some((_, comment)) = line.split_once("//") else { continue };
        let comment = comment.trim();
        if let Some(status) = comment.strip_prefix("expect-exit:") {
            let status = status.trim();
            let status = status.parse().map_err(|_| format!("line {}: invalid exit status `{status}`", i + 1))?;
            if directives.exit.replace(status).is_some() {
                return Err(format!("line {}: more than one `expect-exit`", i + 1));
            }
        } else if let Some(message) = comment.strip_prefix("expect-error:") {
            directives.errors.push(Error { line: Some(i + 1), message: message.trim().to_string() });
        } else if comment.starts_with("expect") {
            return Err(format!("line {}: unknown directive `{comment}`", i + 1));
        }
    }
    Ok(directives)
}

fn check(path: &Path) -> Result<(), String> {
    let source = fs::read_to_string(path).map_err(|err| format!("cannot read file: {err}"))?;
    let directives = parse_directives(&source)?;
    if directives.exit.is_some() && !directives.errors.is_empty() {
        return Err("`expect-exit` and `expect-error` cannot be used in the same file".into());
    }
    if directives.exit.is_none() && directives.errors.is_empty() {
        return Err("no directives; add `// expect-exit: <status>` or `// expect-error: <message>`".into());
    }

    let output = Command::new(COMPILER)
        .arg(path)
        .arg(format!("--emit={LAST_STAGE}"))
        .output()
        .map_err(|err| format!("cannot run compiler: {err}"))?;
    let stderr = String::from_utf8_lossy(&output.stderr);

    if directives.exit.is_some() {
        if !output.status.success() {
            return Err(format!("expected to compile, but compilation failed:\n{stderr}"));
        }
        return Ok(());
    }

    if output.status.success() {
        return Err("expected errors, but compilation succeeded".into());
    }
    compare_errors(directives.errors, reported_errors(&stderr))
        .map_err(|problems| format!("{problems}\ncompiler output:\n{stderr}"))
}

/// Extracts errors from compiler output. A located error looks like:
///
/// ```text
/// error: invalid character '$'
///  --> path/to/file.jo:1:19
/// ```
fn reported_errors(stderr: &str) -> Vec<Error> {
    let mut errors: Vec<Error> = Vec::new();
    for line in stderr.lines() {
        if let Some(message) = line.strip_prefix("error: ") {
            errors.push(Error { line: None, message: message.to_string() });
        } else if let Some(location) = line.trim_start().strip_prefix("--> ") {
            // The location is `path:line:column`; split from the right in case the path has colons.
            let line_number = location.rsplit(':').nth(1).and_then(|n| n.parse().ok());
            if let Some(last) = errors.last_mut() {
                last.line = line_number;
            }
        }
    }
    errors
}

/// Pairs each expected error with a reported one. Fails with a description of every expected error
/// that wasn't reported, and every reported error that wasn't expected.
fn compare_errors(expected: Vec<Error>, mut reported: Vec<Error>) -> Result<(), String> {
    let mut problems = String::new();
    for want in expected {
        let found = reported.iter().position(|got| got.line == want.line && got.message.contains(&want.message));
        match found {
            Some(i) => {
                reported.remove(i);
            }
            None => problems += &format!("missing error on line {}: {}\n", want.line.unwrap(), want.message),
        }
    }
    for got in reported {
        match got.line {
            Some(line) => problems += &format!("unexpected error on line {line}: {}\n", got.message),
            None => problems += &format!("unexpected error: {}\n", got.message),
        }
    }
    if problems.is_empty() { Ok(()) } else { Err(problems) }
}
