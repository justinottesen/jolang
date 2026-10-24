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
process's exit status. Its body can use local variables, assignment, nested blocks, integer
arithmetic, conversions between integer types, booleans, comparisons, and `if`.

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

```jo
fn main() -> u8 {
    let big: u32 = 1000;
    let small = (big / 10) as u8;   // checked: traps if the value doesn't fit
    let wrapped: u8 = 255 +% 1;     // wrapping: 0
    small - 58 + wrapped            // checked: traps on overflow
}
```

```jo
fn main() -> u8 {
    let x: u8 = 7;
    let big = x > 5 and x != 9;
    let y = if big { x * 2 } else { 0 };
    if y == 14 { 0 } else { 1 }
}
```

### Lexical structure

Source files are UTF-8, using the `.jo` extension.

| Element     | Definition                                                     |
|-------------|----------------------------------------------------------------|
| Whitespace  | Space, tab, `\n`, `\r`. Not significant.                       |
| Comment     | `//` to the end of the line.                                   |
| Identifier  | `[A-Za-z_][A-Za-z0-9_]*` (ASCII only for now).                 |
| Keyword     | `fn` `let` `mut` `as` `if` `else` `true` `false` `and` `or`    |
| Integer     | `[0-9]+` (decimal only for now).                               |
| Punctuation | `(` `)` `{` `}` `->` `;` `:` `=`                               |
| Operator    | `+` `-` `*` `/` `%` `+%` `-%` `*%` `as%` `!`                   |
|             | `==` `!=` `<` `<=` `>` `>=`                                    |

Operators ending in `%` are single tokens: `a +% b` is wrapping addition, while `a + %b` is a syntax
error.

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
         | block | if_expr ;             (* a block-like expression needs no `;` *)
expr     = binary ( "=" expr )? ;        (* assignment *)
binary   = cast ( binary_op cast )* ;    (* see the precedence table *)
cast     = unary ( ( "as" | "as%" ) type )* ;
unary    = ( "-" | "-%" | "!" ) unary | primary ;
primary  = INTEGER | "true" | "false" | IDENT | block | if_expr | "(" expr ")" | "(" ")" ;
if_expr  = "if" expr block ( "else" ( if_expr | block ) )? ;
binary_op = "*" | "/" | "%" | "*%" | "+" | "-" | "+%" | "-%"
          | "==" | "!=" | "<" | "<=" | ">" | ">=" | "and" | "or" ;
```

Operator precedence, from tightest to loosest:

| Operators                       | Associativity                |
|---------------------------------|------------------------------|
| unary `-` `-%` `!`              |                              |
| `as` `as%`                      | left                         |
| `*` `/` `%` `*%`                | left                         |
| `+` `-` `+%` `-%`               | left                         |
| `==` `!=` `<` `<=` `>` `>=`     | none: `a < b < c` is an error |
| `and`                           | left                         |
| `or`                            | left                         |
| `=`                             | right                        |

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

**Block-like expressions don't need `;`.** An expression ending in a block (a block or an `if`;
later `while` and so on) can be used as a statement without a trailing `;`, as long as its type is
`()`. Otherwise every `if` used as a statement would need `};` after it. As in Rust, a block-like
expression at the start of a statement is the whole statement: `if c { a } else { b } - 1` is an
`if` statement followed by `-1`, not a subtraction.

**Variables** are declared with `let`, and are immutable unless declared `let mut`. Every `let`
must have an initializer. The type annotation is optional: without one, the variable gets the type
of its initializer. A variable is in scope from the end of its `let` statement to the end of the
enclosing block.

**Shadowing is not allowed.** A variable can't have the same name as another variable that is in
scope, in the same block or an enclosing one. A name always refers to one thing.

**Assignment** (`x = value`) is an expression of type `()`. Its left side must be a variable
declared `mut`. Because its type is `()`, mistakes like `x = y = 5` are type errors.

**Integer literals are untyped.** An integer literal is a constant with no fixed type, and takes its
type from context: a variable's type annotation, the variable being assigned to, a function's
return type, the other operand of an arithmetic operator, or the target of a conversion. In
`x + 1`, the `1` gets the type of `x`. Using a literal where no type can be determined is an error
(for example, `let x = 5;` or `let x = 5 + 3;`), and so is a value that doesn't fit the type it's
given.

A `-` directly in front of a literal is part of the literal, so `-128` is a valid `i8` even though
`128` on its own isn't.

**Integer types** are `u8`, `u16`, `u32`, `u64` (unsigned) and `i8`, `i16`, `i32`, `i64`
(signed, two's complement). Internally a type is "signed or unsigned, plus a bit count", so other
widths (Zig-style `u7`, `i13`) can be added later; they will likely arrive with packed structs,
where they matter most.

**Arithmetic is checked by default.** `+`, `-`, `*`, and unary `-` trap if the result doesn't fit
in the type, in every build mode. This is unlike Zig, where overflow on `+` is undefined behavior
in its fastest build mode, and unlike Rust, which wraps silently in release builds. The check can be
removed only when the compiler proves overflow impossible (not implemented yet).

**Wrapping arithmetic is explicit.** `+%`, `-%`, `*%`, and unary `-%` wrap around on overflow
instead of trapping. They are for code where wrapping is the desired behavior, or performance
critical code where the check is too expensive.

**Division** `/` rounds toward zero, and `%` is the remainder that goes with it, so `-7 / 2` is
`-3` and `-7 % 2` is `-1`. Both trap on division by zero. For signed types both also trap on
`MIN / -1` and `MIN % -1`, since the quotient doesn't fit. There are no wrapping forms.

**Unary `-` and `-%` only apply to signed types.**

**Both operands of an arithmetic operator must have the same type**, and the result has that type
too. There are no implicit conversions between integer types, not even ones that always fit:
`a + b` with `a: u8` and `b: u16` is an error. Following "start strict", this may be relaxed later
for conversions that can never lose information.

**Conversions** between integer types are written `value as T`, and are checked: they trap if the
value doesn't fit in `T`. `value as% T` wraps instead, keeping the low bits of the value
(sign-extending first when converting a signed value to a wider type). A conversion that can never
fail, like `u8` to `u32`, is never checked.

**Traps** end the process immediately, with no unwinding: nothing else in the program runs
afterward. For now, a trap kills the process with `SIGILL`. It will become a panic that reports a
message and the source location.

**`bool`** has two values, `true` and `false`. There is no conversion between `bool` and the
integer types in either direction, and nothing else is "truthy": a condition must be a `bool`.

**Comparisons** `==` and `!=` apply to integers and to `bool`; `<`, `<=`, `>`, and `>=` apply to
integers only. Both operands must have the same type, and the result is a `bool`. Comparisons
can't be chained: `a < b < c` is a syntax error, since it almost never means what it looks like.
Write `a < b and b < c`.

**`and` and `or` short-circuit**: `a and b` only evaluates `b` if `a` is `true`, and `a or b`
only evaluates `b` if `a` is `false`. Because they decide whether code runs, they are control
flow, and are spelled as keywords rather than symbols (as in Zig) so that reads as such. `!` is
logical not. All three apply to `bool` only.

**`if` is an expression.** `if cond { a } else { b }` evaluates the condition, which must be a
`bool`, then the matching branch, and its value is that branch's value. Both branches must have
the same type, which is the type of the `if`. The braces are always required, and no parentheses
are needed around the condition. `else if` chains another `if`. An `if` without `else` has type
`()`, so its block must have type `()` too.

**`main` returns the process exit status.** The program must contain exactly one function, named
`main`, which takes no parameters and returns `u8`. Its return value becomes the process's exit
status. `u8` is used because Linux only keeps the low 8 bits of an exit status, and silently
truncating a wider value would be a hidden behavior.

**Built-in types.** The integer types, `bool`, and `()`. The unit value is written `()`.

### Errors and warnings

The following are compile errors in this slice:

- A program with no `main`, more than one function, or a function not named `main`.
- A return type other than `u8` for `main`, or a type name that doesn't exist.
- An integer literal that doesn't fit in its type (for example, `256` as a `u8`), or whose type
  can't be determined.
- A type mismatch, such as a block with no final expression where a `u8` is expected, operands
  of an operator with different types, `if` branches with different types, or a condition that
  isn't a `bool`.
- An `if` without `else` whose block isn't `()`.
- Arithmetic on anything but integers, negating an unsigned value, ordering comparisons (`<` and
  so on) on `bool`, logical operators on anything but `bool`, or converting anything other than an
  integer.
- Chained comparisons.
- An expression statement whose type isn't `()`.
- Using a variable that isn't defined, or defining a variable whose name is already in scope.
- Assigning to a variable not declared `mut`, or to something that isn't a variable.

Warnings don't stop compilation. They flag code that is legal but probably a mistake:

- A variable that is never read.
- A variable declared `mut` that is never assigned to.

These are checked at runtime, and trap:

- Overflow in `+`, `-`, `*`, or unary `-`.
- Division or remainder by zero, and signed `MIN / -1` or `MIN % -1`.
- A conversion with `as` whose value doesn't fit in the target type.

A constant expression that is certain to trap, like `255 + 1` as a `u8`, still compiles and traps
at runtime. It will become a compile error once the compiler can evaluate expressions at compile
time.

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
- **Removing overflow checks that can't fail.** When the compiler can prove an operation can't
  overflow (for example, from the ranges of its operands), the check is dropped. Needs the planned
  SSA optimization stage.
- **Remarks.** A third kind of diagnostic, alongside errors and warnings, off by default and enabled
  with a flag. Remarks report optimizations the compiler wanted to make but couldn't, and why:
  "couldn't remove this overflow check because nothing bounds `x`". LLVM (`-Rpass-missed`) and GCC
  (`-fopt-info-missed`) have the same idea. They would be overwhelming for a person to read
  directly, but are useful input for tools, including AI, that help tune performance.
- **Compile-time evaluation.** Evaluating constant expressions during compilation, so that
  operations certain to trap become compile errors.
- **Saturating arithmetic** (Zig's `+|`, which clamps to the type's range), if it proves useful.
- **Arbitrary-width integers**, likely with packed structs.
