//! Global (cross-module) string interner. pyast trees each have a private
//! interner; the engine translates per-tree `Sym`s to global `GSym`s once
//! at module-load time (see graph.rs).

use rustc_hash::FxHashMap;

use crate::value::GSym;

#[derive(Default)]
pub struct GlobalInterner {
    map: FxHashMap<Box<str>, GSym>,
    vec: Vec<Box<str>>,
}

impl GlobalInterner {
    pub fn intern(&mut self, s: &str) -> GSym {
        if let Some(&sym) = self.map.get(s) {
            return sym;
        }
        let sym = self.vec.len() as GSym;
        let boxed: Box<str> = s.into();
        self.vec.push(boxed.clone());
        self.map.insert(boxed, sym);
        sym
    }
    pub fn get(&self, sym: GSym) -> &str {
        &self.vec[sym as usize]
    }
    pub fn lookup(&self, s: &str) -> Option<GSym> {
        self.map.get(s).copied()
    }
}
