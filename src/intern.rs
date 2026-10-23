//! Interning: storing each distinct string once and referring to it by a small id.

use std::collections::HashMap;

/// An interned string. Two symbols from the same `Interner` are equal exactly when their strings
/// are, so comparing names is an integer comparison.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Symbol(u32);

#[derive(Default)]
pub struct Interner {
    // Each string is stored twice, once as a key and once for lookup by id. Avoiding that needs
    // either unsafe code or a crate, which isn't worth it for the bootstrap compiler.
    ids: HashMap<String, Symbol>,
    strings: Vec<String>,
}

impl Interner {
    pub fn intern(&mut self, string: &str) -> Symbol {
        if let Some(&symbol) = self.ids.get(string) {
            return symbol;
        }
        let symbol = Symbol(self.strings.len() as u32);
        self.ids.insert(string.to_string(), symbol);
        self.strings.push(string.to_string());
        symbol
    }

    pub fn get(&self, symbol: Symbol) -> &str {
        &self.strings[symbol.0 as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_string_same_symbol() {
        let mut interner = Interner::default();
        let a = interner.intern("main");
        let b = interner.intern("other");
        assert_eq!(interner.intern("main"), a);
        assert_ne!(a, b);
        assert_eq!(interner.get(a), "main");
        assert_eq!(interner.get(b), "other");
    }
}
