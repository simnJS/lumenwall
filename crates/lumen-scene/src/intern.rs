//! String interning: names, paths, shader and texture names are stored once and referenced by a
//! 4-byte [`Sym`].

use std::collections::HashMap;
use std::sync::Arc;

/// Interned string handle. [`Sym::EMPTY`] is the empty string.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Sym(u32);

impl Sym {
    /// The empty string.
    pub const EMPTY: Sym = Sym(0);

    /// Index in the interner.
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// String table owned by a [`crate::Project`].
#[derive(Clone, Debug)]
pub struct Interner {
    strings: Vec<Arc<str>>,
    lookup: HashMap<Arc<str>, Sym>,
}

impl Default for Interner {
    fn default() -> Self {
        Self::new()
    }
}

impl Interner {
    /// A table holding only the empty string.
    pub fn new() -> Self {
        let empty: Arc<str> = Arc::from("");
        let mut lookup = HashMap::new();
        lookup.insert(empty.clone(), Sym::EMPTY);
        Self {
            strings: vec![empty],
            lookup,
        }
    }

    /// Returns the handle of `s`, adding it if needed.
    pub fn intern(&mut self, s: &str) -> Sym {
        if let Some(&sym) = self.lookup.get(s) {
            return sym;
        }
        // Four billion distinct strings cannot fit in memory before this wraps.
        let sym = Sym(self.strings.len() as u32);
        let a: Arc<str> = Arc::from(s);
        self.strings.push(a.clone());
        self.lookup.insert(a, sym);
        sym
    }

    /// Handle of `s` if it was interned.
    pub fn get(&self, s: &str) -> Option<Sym> {
        self.lookup.get(s).copied()
    }

    /// The string of a handle (empty for a handle from another table).
    pub fn resolve(&self, sym: Sym) -> &str {
        self.strings.get(sym.index()).map_or("", |s| s)
    }

    /// Number of distinct strings.
    pub fn len(&self) -> usize {
        self.strings.len()
    }

    /// Always false: the empty string is always present.
    pub fn is_empty(&self) -> bool {
        false
    }
}

impl std::ops::Index<Sym> for Interner {
    type Output = str;

    fn index(&self, sym: Sym) -> &str {
        self.resolve(sym)
    }
}
