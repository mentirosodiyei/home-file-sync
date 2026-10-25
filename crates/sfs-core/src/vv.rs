//! Vectores de versión (la misma idea que usa Syncthing).
//!
//! Cada archivo lleva un contador por equipo. Cuando un equipo modifica el
//! archivo incrementa su propio contador. Comparando dos vectores sabemos si
//! una versión es posterior a la otra (actualización normal) o si se crearon
//! de forma independiente (conflicto).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionVector(pub BTreeMap<u64, u64>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Causality {
    Equal,
    /// `self` contiene todos los cambios de `other` y alguno más.
    Newer,
    /// `other` contiene todos los cambios de `self` y alguno más.
    Older,
    /// Cambios independientes en ambos lados.
    Concurrent,
}

impl VersionVector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn bump(&mut self, device: u64) {
        *self.0.entry(device).or_insert(0) += 1;
    }

    pub fn merge(&mut self, other: &VersionVector) {
        for (d, c) in &other.0 {
            let e = self.0.entry(*d).or_insert(0);
            if *c > *e {
                *e = *c;
            }
        }
    }

    pub fn compare(&self, other: &VersionVector) -> Causality {
        let mut greater = false;
        let mut less = false;
        for (d, c) in &self.0 {
            let o = other.0.get(d).copied().unwrap_or(0);
            if *c > o {
                greater = true;
            } else if *c < o {
                less = true;
            }
        }
        for (d, o) in &other.0 {
            if !self.0.contains_key(d) && *o > 0 {
                less = true;
            }
        }
        match (greater, less) {
            (false, false) => Causality::Equal,
            (true, false) => Causality::Newer,
            (false, true) => Causality::Older,
            (true, true) => Causality::Concurrent,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn causality() {
        let mut a = VersionVector::new();
        a.bump(1);
        let mut b = a.clone();
        assert_eq!(a.compare(&b), Causality::Equal);
        b.bump(2);
        assert_eq!(b.compare(&a), Causality::Newer);
        assert_eq!(a.compare(&b), Causality::Older);
        a.bump(1);
        assert_eq!(a.compare(&b), Causality::Concurrent);
        let mut m = a.clone();
        m.merge(&b);
        assert_eq!(m.compare(&a), Causality::Newer);
        assert_eq!(m.compare(&b), Causality::Newer);
    }
}
