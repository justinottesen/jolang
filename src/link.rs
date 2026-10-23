//! Turns assembly into an executable, using the system's GNU assembler and linker.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Assembles `assembly` and links it into a static executable at `output`.
pub fn assemble_and_link(assembly: &str, output: &Path) -> Result<(), String> {
    // The object file goes next to the output, and is removed afterward.
    let mut object = OsString::from(output);
    object.push(".o");
    let object = PathBuf::from(object);

    let result = assemble(assembly, &object).and_then(|()| link(&object, output));
    let _ = std::fs::remove_file(&object);
    result
}

fn assemble(assembly: &str, object: &Path) -> Result<(), String> {
    // `-` reads the assembly from stdin, so it never needs to be written to a file.
    let mut child = Command::new("as")
        .args(["--64", "-o"])
        .arg(object)
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("could not run the assembler `as`: {err}"))?;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(assembly.as_bytes())
        .map_err(|err| format!("could not send assembly to `as`: {err}"))?;
    let result = child.wait_with_output().map_err(|err| format!("could not run the assembler `as`: {err}"))?;
    if !result.status.success() {
        return Err(format!("the assembler failed:\n{}", String::from_utf8_lossy(&result.stderr)));
    }
    Ok(())
}

fn link(object: &Path, output: &Path) -> Result<(), String> {
    let result = Command::new("ld")
        .arg("-static")
        .arg("-o")
        .arg(output)
        .arg(object)
        .output()
        .map_err(|err| format!("could not run the linker `ld`: {err}"))?;
    if !result.status.success() {
        return Err(format!("the linker failed:\n{}", String::from_utf8_lossy(&result.stderr)));
    }
    Ok(())
}
