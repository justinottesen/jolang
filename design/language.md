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

### Safe defaults

When there is a choice, the default is the safer option, and the less safe one must be asked for
explicitly. Variables are immutable unless declared `mut`; a value can't be silently discarded.

Where it's unclear whether to allow something, start strict. Relaxing a rule later breaks no
existing code; tightening one does.

### Fast to compile

Compile time is a first-class concern, even though the bootstrap compiler doesn't optimize for it.
Language features should not make fast or incremental compilation impossible. In particular:

- A file can be parsed without information from any other file. There are no textual includes, and
  no constructs whose parse depends on name resolution.
- The design should allow work to be cached and redone per declaration, rather than per file or per
  program.

## Current Slice

For now, the language is very small. A program is a single function, `main`, which returns the
process's exit status. Its body can use local variables, assignment, and nested blocks.

```jo
fn main() -> u8 {
    let x: u8 = 40;
    let y = x;           // the type of `y` comes from `x`
    let mut z: u8 = 1;
    z = y;
    {
        let inner: u8 = z;
        inner
    }
}
```

### Lexical structure

Source files are UTF-8, using the `.jo` extension.

| Element     | Definition                                                     |
|-------------|----------------------------------------------------------------|
| Whitespace  | Space, tab, `\n`, `\r`. Not significant.                       |
| Comment     | `//` to the end of the line.                                   |
| Identifier  | `[A-Za-z_][A-Za-z0-9_]*` (ASCII only for now).                 |
| Keyword     | `fn` `let` `mut`                                               |
| Integer     | `[0-9]+` (decimal only for now).                               |
| Punctuation | `(` `)` `{` `}` `->` `;` `:` `=`                               |

Type names such as `u8` are ordinary identifiers that resolve to built-in types, not keywords. This
keeps built-in and user-defined types uniform, and lets new types be added without touching the
lexer.

### Grammar

```ebnf
file     = fn_decl* ;
fn_decl  = "fn" IDENT "(" ")" "->" type block ;
type     = IDENT | "(" ")" ;
block    = "{" stmt* expr? "}" ;
stmt     = "let" "mut"? IDENT ( ":" type )? "=" expr ";"
         | expr ";"
         | block ;                       (* a block-like expression needs no `;` *)
expr     = primary ( "=" expr )? ;       (* assignment *)
primary  = INTEGER | IDENT | block ;
```

The grammar accepts more than the language allows (any number of functions, any name, any type
name, assigning to things that aren't variables). Those restrictions are checked after parsing, so
they can produce specific error messages.

### Semantics

**Almost everything is an expression.** Following Rust, the only statements are `let` and
expression statements; everything else, including assignment, is an expression with a type and a
value. `let` has to be a statement, because the name it introduces stays visible for the rest of
the block.

**Blocks are expressions.** A block is a list of statements, optionally followed by a final
expression. Its value is the value of the final expression, or `()` if there isn't one. A
function's body is a block, and its value is the function's return value.

**The unit type `()`** has exactly one value and carries no information. It is the type of
expressions that are run only for their effect, like assignment, and of blocks without a final
expression.

**Values can't be silently discarded.** An expression statement (`expr;`) must have type `()`.
Discarding any other value is an error, since silently ignoring a result is a common source of bugs.
A way to discard a value on purpose (likely `_ = expr;`) will come with function calls, when it
becomes useful.

**Block-like expressions don't need `;`.** An expression ending in a block (for now, just a block;
later `if`, `while`, and so on) can be used as a statement without a trailing `;`, as long as its
type is `()`. Otherwise every `if` used as a statement would need `};` after it.

**Variables** are declared with `let`, and are immutable unless declared `let mut`. Every `let`
must have an initializer. The type annotation is optional: without one, the variable gets the type
of its initializer. A variable is in scope from the end of its `let` statement to the end of the
enclosing block.

**Shadowing is not allowed.** A variable can't have the same name as another variable that is in
scope, in the same block or an enclosing one. A name always refers to one thing.

**Assignment** (`x = value`) is an expression of type `()`. Its left side must be a variable
declared `mut`. Because its type is `()`, mistakes like `x = y = 5` are type errors.

**Integer literals are untyped.** An integer literal is a constant with no fixed type, and takes its
type from context: a variable's type annotation, the variable being assigned to, or a function's
return type. Using a literal where no type can be determined is an error (for example,
`let x = 5;`), and so is a value that doesn't fit the type it's given.

**`main` returns the process exit status.** The program must contain exactly one function, named
`main`, which takes no parameters and returns `u8`. Its return value becomes the process's exit
status. `u8` is used because Linux only keeps the low 8 bits of an exit status, and silently
truncating a wider value would be a hidden behavior.

**Built-in types.** `u8`, an unsigned 8-bit integer, and `()`.

### Errors and warnings

The following are compile errors in this slice:

- A program with no `main`, more than one function, or a function not named `main`.
- A return type other than `u8` for `main`, or a type name that doesn't exist.
- An integer literal that doesn't fit in its type (for example, `256` as a `u8`), or whose type
  can't be determined.
- A type mismatch, such as a block with no final expression where a `u8` is expected.
- An expression statement whose type isn't `()`.
- Using a variable that isn't defined, or defining a variable whose name is already in scope.
- Assigning to a variable not declared `mut`, or to something that isn't a variable.

Warnings don't stop compilation. They flag code that is legal but probably a mistake:

- A variable that is never read.
- A variable declared `mut` that is never assigned to.

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
