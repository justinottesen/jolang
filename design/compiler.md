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

Errors are phrased as `expected <what the grammar needs>, found <the actual token>`. The error
points at the unexpected token, unless that token is on a later line or is the end of the file. In
those cases it points just after the previous token, since that is where the missing code belongs:
a forgotten `;` at the end of a line is reported at the end of that line, not at the start of the
next.

Inside a block, an expression followed by `}` is the block's final expression; followed by `;` it
is a statement. A block at the start of a statement always ends the statement, so `{ ... } = 1`
does not parse as an assignment. Rust parses it the same way.

The parser accepts more than the language allows (see the grammar in [language.md](language.md)).
Rules like "exactly one function, named `main`" are the checker's job.

### Checker (AST -> HIR)

The checker validates the program and produces a new, separate tree: the HIR (high-level
intermediate representation). The AST describes what was written; the HIR describes what it means:
names are resolved, every expression has a type, and untyped literals have been given concrete
types.

The checker builds a new tree rather than annotating the AST, so that later stages never deal with
raw syntax. As the language grows, sugar (convenient syntax that is shorthand for something
simpler) will be removed at this stage. A HIR only exists for a program with no errors, so later
stages can assume it is valid.

**Type checking is bidirectional.** Types flow in two directions:

- *Checking*: the context says what type an expression must have, and the expression is checked
  against it. A function body is checked against the declared return type; an untyped integer
  literal gets its type this way.
- *Inference*: the expression's type is computed from its parts, bottom-up. A variable reference has
  the type of the variable, and an assignment has type `()`. A `let` without a type annotation
  infers its initializer.

Checking an expression that has no special checking rule falls back to inferring its type and
comparing it with the expected one.

An untyped literal with no context to check it against is an error. The alternative design is
Rust-style inference, where the checker creates unknown types and solves for them using
constraints from across the whole function. That can infer more, but a type may then depend on code
far from where it's used. Bidirectional checking keeps every type determined by nearby code, which
fits the explicitness goal and keeps it cheap for tools to show inferred types.

**The checker reports every error it can**, unlike the parser. When a part of the program fails to
check, the code that depends on it is skipped, so one mistake doesn't cause a cascade of follow-on
errors. In the code, a function that has reported an error returns a `Reported` value instead of a
result, which tells its callers an error has already been reported and what depends on it should be
skipped.

A variable whose declaration failed (for example, `let x = 5;`, whose type can't be inferred) is
still put in scope, but marked as *poisoned*. Using it later is skipped silently instead of
reporting "undefined variable", since the real problem was already reported.

**Scopes** are a stack of name-to-variable maps, one per block. Looking up a name searches from the
innermost scope out. Because shadowing is not allowed, declaring a name that is found anywhere in
the stack is an error.

**Warnings** for unused variables, and `mut` variables never assigned to, are reported when a
variable's scope ends, using usage flags recorded as the block was checked. A program with only
warnings still compiles. The checker sorts all its diagnostics by position before returning them,
since warnings are found after the errors inside their scope.

Names are interned: each distinct identifier is stored once, and the HIR refers to it by a `Symbol`
id. Each HIR function lists every local variable declared in it, and expressions refer to them by
index.

### Lowering (HIR -> IR)

The IR represents each function as a control-flow graph: a set of basic blocks, each a straight-line
list of statements ending in a terminator (`return`, a jump, a branch). Ownership and linear-type
checking will run on this representation, because it makes every possible path through a function
explicit.

**The IR is built on places, like Rust's MIR.** Each function has a list of *locals*: typed storage
slots, standing for variables and temporaries. Statements assign values to *places*: a local, or
eventually part of one (a field, the target of a pointer, an array element). A place can be
assigned more than once. By convention, local `_0` holds the function's return value, so
`return` takes no operand.

```
fn main() -> u8 {
    let _0: u8
  bb0:
    _0 = const 42_u8
    return
}
```

This was chosen over SSA (every value defined exactly once, as in LLVM) because:

- Linear types are a question about places: "on every path to the end of the scope, was the
  resource in `x` consumed?" On this IR that is a standard dataflow analysis, and its errors can
  refer to things the user wrote.
- Lowering the HIR to places is straightforward, as is naive codegen that gives every local a
  stack slot.

SSA is much better for optimization. When generated code quality starts to matter, an SSA IR will be
added as a new stage between this IR and codegen, the same split Rust makes between MIR and LLVM.

Lowering uses *destination passing*: an expression is lowered along with the place its value should
be stored in. A function body is lowered into `_0`, a `let` lowers its initializer into the
variable's local, and a block passes its destination on to its final expression. This avoids
creating a temporary for every intermediate value. An expression statement is lowered with no
destination, since its value (always `()`) is discarded.

Each HIR variable becomes an IR local, after `_0`. Reading a variable is a `copy` operand. Rust's
MIR also has a `move` operand, which leaves the source unusable; linear types will need it, but
every type can be freely copied so far.

Assignment is the one place destination passing needs care, because the target already holds a
value the right-hand side might read. A constant or a variable is stored into the target directly.
Anything else (for now, a block) is evaluated into a fresh temporary first and then copied, so the
target never holds a half-computed value while the right-hand side is still running. With
multi-statement values like struct literals, writing directly into the target could otherwise read
fields that have already been overwritten.

Values of type `()` carry no data, so storing one does nothing, and their locals take no space.

### Codegen and printing

Codegen produces a list of structured x86-64 instructions (a Rust enum), not text. The printer turns
that list into GNU assembler syntax using `.intel_syntax noprefix`.

Keeping instructions structured means that writing our own assembler later only requires encoding
the same list to bytes. The rest of the pipeline is unaffected. The same goes for writing an ELF
writer to replace `ld`. Both are planned for the self-hosted compiler, not the bootstrap.

Codegen is deliberately naive. Every IR local gets its own slot in the function's stack frame, and
each statement reads from and writes to those slots directly, with no register allocation. A
function looks like:

```asm
main:
    push rbp                    ; prologue: save the caller's frame pointer
    mov rbp, rsp
    sub rsp, 16                 ; reserve the frame; locals are at negative offsets from rbp
.Lmain.bb0:                     ; one label per IR block
    mov byte ptr [rbp - 1], 42  ; _0 = const 42_u8
    mov al, byte ptr [rbp - 1]  ; return: load _0 into the return register
    mov rsp, rbp                ; epilogue: free the frame, restore the caller's frame pointer
    pop rbp
    ret
```

x86 can't move directly from memory to memory, so `_1 = copy _2` becomes a load into `rax` and a
store from it. Locals of type `()` get no slot at all.

The frame size is rounded up to a multiple of 16, so `rsp` stays aligned as the System V ABI
requires at calls. Better code comes later, from the SSA stage and a real register allocator.

Building an executable pipes the assembly into `as`, which writes an object file next to the output
(`<output>.o`), then runs `ld` on it and deletes the object file. `--emit=asm` prints the assembly
instead.

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

Warnings are marked the same way, with `expect-warning`. Since warnings don't stop compilation,
they can appear in programs that also use `expect-exit`:

```jo
// expect-exit: 0
fn main() -> u8 {
    let x: u8 = 1;  // expect-warning: unused variable `x`
    0
}
```

Every program's diagnostics must match its directives exactly, so an unexpected warning fails the
test too. A file cannot contain both `expect-exit` and `expect-error`. Each line can only hold one
directive, so a test shouldn't produce two diagnostics on the same line.

The test runner is [tests/corpus.rs](../tests/corpus.rs). It runs with `cargo test`, and like other
tests can be filtered by name, for example `cargo test lexer` runs only the corpus files whose path
contains `lexer`.

Every program is compiled to an executable. `expect-exit` programs are then run, and their exit
status compared. A program killed by a signal (for example, a segfault in generated code) fails
with the signal number.

Individual stages, like the lexer and parser, also get unit tests.
