# jolang - Justin Ottesen's Language

> [!WARNING]
> This project is pre-alpha, see the [Roadmap](#roadmap) for more details.

Jolang is a personal programming language. The goal is to provide high-level ergonomics without
sacrificing low level hardware control. It takes heavy inspiration from C, C++, Rust, and Zig. The
main goal of this project is learning, the outcome of a usable language is a side effect.

Some properties I want the resulting language to have:
- **Explicit** - Everything programs in the language do should be clear to the programmer. No
  garbage collection pauses, hidden heap allocations, implicit destructors, operator overloading,
  etc.
- **Safety** - Programs that successfully compile shouldn't break any invariants the writer expects
  to hold. This is partially on the developer, but the language must provide powerful tools for
  compile time guarantees.

# Roadmap

This project is in active development. No completed releases are available yet.

## Bootstrapped Compiler

The project will start with a bootstrapped compiler in Rust. The first goal is a simple compiler to
build out from, missing a lot of the eventually desired language features.