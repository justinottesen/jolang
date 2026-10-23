# Compiler Design

This document outlines the design of the compiler. It describes the bootstrap compiler, written in
Rust, which exists to get Jolang far enough along to compile a compiler written in Jolang. The
bootstrap compiler is expected to be thrown away after that, so it should stay small. The design
ideas here are meant to carry over to the self-hosted compiler.

## Pipeline

The compiler is a pipeline of transformations, each one producing a new representation:

```mermaid
flowchart LR

Source -->|Lexer| Tokens -->|Parser| AST -->|Checker| HIR -->|Lowering| IR -->|Codegen| Instructions
Instructions -->|Printer| Assembly -->|as| ObjectFile["Object File"] -->|ld| Executable
```

| Stage    | Input -> Output       | Responsibility                                                    |
|----------|-----------------------|-------------------------------------------------------------------|
| Lexer    | Source -> Tokens      | Split text into tokens. Hand-written.                             |
| Parser   | Tokens -> AST         | Build a syntax tree. Hand-written recursive descent.              |
| Checker  | AST -> HIR            | Resolve names, check types, enforce language rules.               |
| Lowering | HIR -> IR             | Turn structured code into a control-flow graph.                   |
| Codegen  | IR -> Instructions    | Choose x86-64 instructions.                                       |
| Printer  | Instructions -> text  | Print GNU assembler syntax.                                       |
| `as`     | Assembly -> object    | External: GNU assembler.                                          |
| `ld`     | Object -> executable  | External: GNU linker.                                             |

### Lexer

The lexer turns the whole file into a list of tokens up front. A token holds only its kind and its
span. Its text is sliced from the source when needed, never copied. On an invalid character the lexer
reports an error and keeps going, so one typo doesn't hide others.

### Parser

The parser builds the AST, recording a span on every node. For now it stops at the first syntax
error; error recovery can come later.

Errors are phrased as `expected <what the grammar needs>, found <the actual token>`. When the parser
runs out of input, the error points just after the last token rather than at the end of the file,
since that is where the missing code belongs.

The parser accepts more than the language allows (see the grammar in [language.md](language.md)).
Rules like "exactly one function, named `main`" are the checker's job.

### Checker (AST -> HIR)

The checker validates the program and produces a new, separate tree: the HIR (high-level
intermediate representation). The AST describes what was written; the HIR describes what it means:
names are resolved, every expression has a type, and untyped literals have been given concrete
types.

The checker builds a new tree rather than annotating the AST, so that later stages never deal with
raw syntax. As the language grows, sugar (convenient syntax that is shorthand for something
simpler) will be removed at this stage.

### Lowering (HIR -> IR)

The IR represents each function as a control-flow graph: a set of basic blocks, each a straight-line
list of operations ending in a terminator (`return`, a jump, a branch). In this slice every function
is a single block ending in `return <constant>`, but the shape is the one later analyses need.
Ownership and linear-type checking will run on this representation, because it makes every
possible path through a function explicit.

**Open question:** whether the IR is in SSA form (every value assigned exactly once) or built on
named locals and memory locations, like Rust's MIR. This affects how ownership analysis is written.
Nothing in the current slice forces the decision.

### Codegen and printing

Codegen produces a list of structured x86-64 instructions (a Rust enum), not text. The printer turns
that list into GNU assembler syntax using `.intel_syntax noprefix`.

Keeping instructions structured means that writing our own assembler later only requires encoding
the same list to bytes. The rest of the pipeline is unaffected. The same goes for writing an ELF
writer to replace `ld`. Both are planned for the self-hosted compiler, not the bootstrap.

## Target and Runtime

The only target is **x86-64 Linux**, producing a statically linked ELF executable. There is no libc.

The compiler emits the program's entry point, `_start`, which calls `main` and passes its result to
the `exit_group` syscall:

```asm
_start:
    call main
    movzx edi, al       ; main returns u8 in al; the rest of rax is unspecified
    mov eax, 231        ; exit_group
    syscall
```

Functions use the System V calling convention for now. Program arguments and the environment, which
the kernel passes on the stack, are ignored in this slice.

Everything target-specific (entry point, syscalls, object format, calling convention) should live
behind a single target boundary, so that additional targets (other OSes, UEFI, BIOS) can be added
without touching the rest of the compiler.

## Data Representation

### Spans and diagnostics

Every source file gets a numeric id. A `Span` is a file id plus start and end byte offsets. Line and
column numbers are computed only when a diagnostic is printed.

A diagnostic has a severity, a message, and a primary span, and is printed with the relevant source
line underneath it.

### Designing for performance later

Performance is not a goal for the bootstrap compiler, but its structure should not get in the way
of a fast, incremental compiler later. Zig's compiler is the main model. The habits to start with:

- **Index-based storage.** Nodes of the AST, HIR, and IR are stored in flat arrays and refer to each
  other by integer index (with a distinct index type per array), rather than through individually
  allocated, pointer-linked nodes. This is cache-friendly, cheap to allocate and free, and easy to
  serialize to a cache. It also translates directly into Jolang, where pointer-heavy graphs would
  fight against linear types.
- **Interning.** Identifiers and types are stored once and referred to by id, so comparing them is
  an integer comparison.
- **No global state.** Each stage is a function of its explicit inputs. This is what makes it
  possible to later cache a stage's result and reuse it when the inputs haven't changed.
- **Per-declaration granularity.** Later stages should treat each top-level declaration as an
  independent unit of work. That is the granularity incremental compilation will cache at.

Actual caching, parallelism, and incremental compilation are out of scope for the bootstrap.

## Command Line

```sh
jolang <file.jo> -o <output>
jolang <file.jo> --emit=<stage>    # stage: tokens, ast, hir, ir, asm
```

`--emit` prints the output of a single stage, which is useful for debugging and for seeing how each
stage works.

## Dependencies

The bootstrap compiler uses only the Rust standard library. It runs the system `as` and `ld` as
external programs.

## Testing

The main test suite is a corpus of Jolang programs in `tests/corpus/`, organized into directories by
topic. Each program says what should happen to it using comments, called directives.

A program that should compile and run says what exit status it should produce, usually at the top of
the file:

```jo
// expect-exit: 42
fn main() -> u8 { 42 }
```

A program that should fail to compile marks each error it expects **on the line where the error
occurs**. The test passes only if the compiler reports exactly those errors: each one on the right
line, with a message containing the given text, and no others.

```jo
fn main() -> u8 { 256 }  // expect-error: does not fit
```

A file cannot contain both kinds of directive.

The test runner is [tests/corpus.rs](../tests/corpus.rs). It runs with `cargo test`, and like other
tests can be filtered by name, for example `cargo test lexer` runs only the corpus files whose path
contains `lexer`.

**Until the compiler can build executables**, `expect-exit` programs are only run through the latest
implemented stage, and pass if they get there without errors. The stage is a constant in the test
runner, updated as each stage is added.

Individual stages, like the lexer and parser, also get unit tests.
