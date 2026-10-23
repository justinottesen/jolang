# Language Design

This document outlines the design of the language: the principles behind it, what is currently
implemented, and the direction it is expected to take.

## Principles

### Explicit

Jolang follows Zig's definition: **no hidden control flow, no hidden allocations, no hidden cost.**
If something happens at runtime, it should be visible in the source code that causes it.

This does not mean everything has to be written out by hand. Things the compiler can determine
unambiguously, like the type of a local variable, may be inferred. Tooling is expected to make
inferred information visible (for example, LSP inlay hints for inferred types). The language should
be designed so that inferred information is cheap for tools to compute.

### Memory safe without a garbage collector

Programs that compile should not violate memory safety. Resources will be tracked with linear types:
the compiler rejects a program that lets a resource go out of scope without it being explicitly
consumed (freed, closed, moved, returned, ...). The compiler checks that the cleanup happens; it never
inserts the cleanup itself. See [Future Direction](#future-direction).

### Slices, not terminators

There is no null termination anywhere in the language. Strings and buffers are slices (a pointer and
a length). There are also no null pointers; a value that may be absent will be represented with an
optional type.

Some external systems expect null-terminated data, such as Linux syscalls that take paths, or the
program arguments the kernel passes at startup. Converting to and from slices at those boundaries is
the job of library code, and it happens explicitly. The language itself never assumes a terminator.

### Fast to compile

Compile time is a first-class concern, even though the bootstrap compiler doesn't optimize for it.
Language features should not make fast or incremental compilation impossible. In particular:

- A file can be parsed without information from any other file. There are no textual includes, and
  no constructs whose parse depends on name resolution.
- The design should allow work to be cached and redone per declaration, rather than per file or per
  program.

## Current Slice

For now, the language is incredibly simple. A program is a single function, `main`, which returns a
constant exit status.

```jo
fn main() -> u8 { 0 }
```

### Lexical structure

Source files are UTF-8, using the `.jo` extension.

| Element     | Definition                                                     |
|-------------|----------------------------------------------------------------|
| Whitespace  | Space, tab, `\n`, `\r`. Not significant.                       |
| Comment     | `//` to the end of the line.                                   |
| Identifier  | `[A-Za-z_][A-Za-z0-9_]*` (ASCII only for now).                 |
| Keyword     | `fn`                                                           |
| Integer     | `[0-9]+` (decimal only for now).                               |
| Punctuation | `(` `)` `{` `}` `->`                                           |

Type names such as `u8` are ordinary identifiers that resolve to built-in types, not keywords. This
keeps built-in and user-defined types uniform, and lets new types be added without touching the
lexer.

### Grammar

```ebnf
file     = fn_decl* ;
fn_decl  = "fn" IDENT "(" ")" "->" type block ;
type     = IDENT ;
block    = "{" expr "}" ;
expr     = INTEGER ;
```

The grammar accepts more than the language allows (any number of functions, any name, any type
name). Those restrictions are checked after parsing, so they can produce specific error messages.

### Semantics

**Blocks are expressions.** A block evaluates to the value of its final expression. A function's
body is a block, and its value is the function's return value. In this slice, a block contains
exactly one expression.

**Integer literals are untyped.** An integer literal is a constant with no fixed type, and takes its
type from context. Using it where no type can be determined is an error, and so is a value that
doesn't fit the type it's given. In this slice, the only context is the function's return type.

**`main` returns the process exit status.** The program must contain exactly one function, named
`main`, which takes no parameters and returns `u8`. Its return value becomes the process's exit
status. `u8` is used because Linux only keeps the low 8 bits of an exit status, and silently
truncating a wider value would be a hidden behavior.

**Built-in types.** The only type is `u8`, an unsigned 8-bit integer.

### Errors

The following are compile errors in this slice:

- A program with no `main`, more than one function, or a function not named `main`.
- A return type other than `u8`, or a type name that doesn't exist.
- An integer literal that doesn't fit in its type (for example, `256` as a `u8`).

## Future Direction

None of these are designed yet. They are recorded so that early decisions don't accidentally close
them off.

- **Linear types.** Resources must be explicitly consumed before going out of scope, and the compiler
  checks this. Likely paired with `defer`/`errdefer`-style constructs to keep cleanup on early
  returns readable.
- **Async through explicit capabilities.** Allocators, I/O, and scheduling are passed explicitly as
  parameters rather than provided by an implicit runtime, similar in spirit to Zig's allocator and
  `Io` interfaces. Async must work in freestanding code with no runtime and no hidden allocation.
- **Multiple targets.** Hosted targets (Linux first, then others, with a Jolang OS as the primary
  target eventually) and freestanding targets (UEFI, BIOS boot). The goal is to be able to write an
  entire boot process and build system in Jolang as one cohesive thing.
- **`main` on other targets.** `u8` comes from Linux's exit status; other targets may want a
  different signature for their entry point. To revisit once there is a second target.
- **Calling convention / ABI.** Undecided. The bootstrap compiler uses System V for now; this is not
  a commitment.
