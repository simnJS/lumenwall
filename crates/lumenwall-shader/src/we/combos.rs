//! Combos (spec 03 §5): discovery in a unit and emission of the `#define`s.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt::Write as _;

use serde_json::Value;

use super::annotations::{SamplerParam, leading_int, sampler_slot};
use super::{RawAnnotation, ScanItem};

pub type Combos = BTreeMap<String, i64>;

/// Combos of a pass as provided by the material/scene layer.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct PassCombos {
    /// `combos` of the scene pass override, plus engine-imposed ones (§5.4).
    pub overrides: Combos,
    /// `combos` of the material pass, with `TEX0FORMAT` already applied (§5.4).
    pub material: Combos,
    /// Slots counted as "used" (§5.3): material `textures` ∪ `usertextures`
    /// and the override's `textures`.
    pub used_slots: BTreeSet<u32>,
}

/// Integer value of a `[COMBO]` default. The reference abandons the object for
/// anything but an integer; Lumenwall truncates numbers and maps booleans to 1/0.
fn combo_default(v: Option<&Value>, warnings: &mut Vec<String>, name: &str) -> i64 {
    match v {
        None => 0,
        Some(Value::Number(n)) => match n.as_i64().or_else(|| n.as_u64().map(|u| u as i64)) {
            Some(i) => i,
            None => {
                warnings.push(format!(
                    "combo '{name}': non-integer default {n}, truncated"
                ));
                n.as_f64().unwrap_or(0.0).trunc() as i64
            }
        },
        Some(Value::Bool(b)) => {
            warnings.push(format!("combo '{name}': boolean default"));
            *b as i64
        }
        Some(other) => {
            warnings.push(format!("combo '{name}': invalid default {other}, using 0"));
            0
        }
    }
}

fn require_entry_satisfied(pass: &PassCombos, name: &str, v: &Value) -> bool {
    match (pass.material.get(name), v.as_i64()) {
        (Some(have), Some(want)) => *have == want,
        _ => false,
    }
}

/// §5.3: decides whether a sampler combo is defined, and with which value.
fn texture_combo(s: &SamplerParam, combo: &str, pass: &PassCombos) -> Option<i64> {
    let used = s.slot.is_some_and(|slot| pass.used_slots.contains(&slot));
    let mut required = if used {
        true
    } else if let Some(req) = &s.require {
        // parity: both rules look inverted (open question 1); reproduced as observed.
        if s.require_any {
            req.iter().any(|(n, v)| {
                !pass.material.contains_key(n)
                    || pass.overrides.contains_key(n)
                    || !require_entry_satisfied(pass, n, v)
            })
        } else {
            !req.iter().any(|(n, v)| require_entry_satisfied(pass, n, v))
        }
    } else {
        false
    };
    let mut value = 1;
    if required && !used {
        let explicit = pass.material.contains_key(combo) || pass.overrides.contains_key(combo);
        match &s.raw_default {
            None => required = false,
            Some(_) if explicit => required = false,
            Some(Value::Number(n)) => value = n.as_f64().unwrap_or(1.0).trunc() as i64,
            // Open question 2: the reference abandons the object for a texture
            // name default; Lumenwall enables the combo (V = 1).
            Some(Value::String(t)) => value = leading_int(t).unwrap_or(1),
            Some(_) => {}
        }
    }
    required.then_some(value)
}

/// Combos discovered in one unit (§5.2, §5.3), first declaration wins.
pub fn discover(
    items: &[ScanItem],
    samplers: &[SamplerParam],
    pass: &PassCombos,
    warnings: &mut Vec<String>,
) -> Combos {
    let mut found = Combos::new();
    let mut sampler_iter = samplers.iter();
    for item in items {
        match item {
            ScanItem::Combo(m) => {
                let Some(name) = m.get("combo").and_then(Value::as_str) else {
                    // The reference abandons the object; Lumenwall ignores the declaration.
                    warnings.push("[COMBO] declaration without \"combo\" key ignored".into());
                    continue;
                };
                if pass.material.contains_key(name) || pass.overrides.contains_key(name) {
                    continue;
                }
                let v = combo_default(m.get("default"), warnings, name);
                found.entry(name.to_string()).or_insert(v);
            }
            ScanItem::Annotation(RawAnnotation {
                glsl_type, name, ..
            }) => {
                if glsl_type != "sampler2D" && glsl_type != "sampler2DComparison" {
                    continue;
                }
                let Some(s) = sampler_iter.next() else {
                    continue;
                };
                debug_assert_eq!(&s.uniform, name);
                let Some(combo) = &s.combo else { continue };
                if sampler_slot(name).is_none() {
                    continue;
                }
                if let Some(v) = texture_combo(s, combo, pass) {
                    found.entry(combo.clone()).or_insert(v);
                }
            }
        }
    }
    found
}

/// §5.5: `#define`s of one unit. First emitted (upper-cased) name wins.
pub fn defines(pass: &PassCombos, own: &Combos, other: &Combos) -> String {
    let mut out = String::new();
    let mut seen: HashSet<String> = HashSet::new();
    for group in [&pass.overrides, &pass.material, own, &pass.material, other] {
        for (name, v) in group {
            let up = name.to_ascii_uppercase();
            if seen.insert(up.clone()) {
                let _ = writeln!(out, "#define {up} {v}");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::we::annotations::interpret;
    use crate::we::preprocess_unit;

    fn unit(src: &str) -> (Vec<ScanItem>, Vec<SamplerParam>) {
        let u = preprocess_unit(&crate::fs::MemFs::default(), src).unwrap();
        let raw: Vec<&RawAnnotation> = u
            .items
            .iter()
            .filter_map(|i| match i {
                ScanItem::Annotation(a) => Some(a),
                _ => None,
            })
            .collect();
        let p = interpret(&raw, &mut Vec::new());
        (u.items.clone(), p.samplers)
    }

    #[test]
    fn spec_example_5_6() {
        // Material MASK=0, override textures slot 1, [COMBO] BLENDMODE, g_Texture1 combo MASK.
        let src = "// [COMBO] {\"combo\":\"BLENDMODE\",\"default\":0}\nuniform sampler2D g_Texture1; // {\"combo\":\"MASK\"}\nvoid main() {}\n";
        let (items, samplers) = unit(src);
        let pass = PassCombos {
            overrides: Combos::new(),
            material: [("MASK".to_string(), 0)].into(),
            used_slots: [1].into(),
        };
        let found = discover(&items, &samplers, &pass, &mut Vec::new());
        assert_eq!(found.get("BLENDMODE"), Some(&0));
        assert_eq!(found.get("MASK"), Some(&1));
        let d = defines(&pass, &found, &Combos::new());
        assert_eq!(d, "#define MASK 0\n#define BLENDMODE 0\n");
    }

    #[test]
    fn unused_texture_combo_stays_undefined() {
        let src = "uniform sampler2D g_Texture2; // {\"combo\":\"MASK\",\"default\":\"util/white\"}\nvoid main() {}\n";
        let (items, samplers) = unit(src);
        let found = discover(&items, &samplers, &PassCombos::default(), &mut Vec::new());
        assert!(found.is_empty());
    }

    #[test]
    fn require_rules_reproduce_reference() {
        let src = "uniform sampler2D g_Texture2; // {\"combo\":\"FX\",\"default\":2,\"require\":{\"A\":1}}\nuniform sampler2D g_Texture3; // {\"combo\":\"FY\",\"default\":\"util/x\",\"require\":{\"A\":1},\"requireany\":true}\nvoid main() {}\n";
        let (items, samplers) = unit(src);
        // A absent: FX required (no entry satisfied), FY required (entry unsatisfied, V = 1).
        let found = discover(&items, &samplers, &PassCombos::default(), &mut Vec::new());
        assert_eq!(found.get("FX"), Some(&2));
        assert_eq!(found.get("FY"), Some(&1));
        // A = 1 in the material: both not required.
        let pass = PassCombos {
            material: [("A".to_string(), 1)].into(),
            ..Default::default()
        };
        let found = discover(&items, &samplers, &pass, &mut Vec::new());
        assert!(found.is_empty());
    }

    #[test]
    fn first_declaration_and_case_folding() {
        let src = "// [COMBO] {\"combo\":\"X\",\"default\":1}\n// [COMBO] {\"combo\":\"X\",\"default\":2}\n// [COMBO] {\"combo\":\"y\"}\nvoid main() {}\n";
        let (items, samplers) = unit(src);
        let pass = PassCombos {
            overrides: [("Y".to_string(), 5)].into(),
            ..Default::default()
        };
        let found = discover(&items, &samplers, &pass, &mut Vec::new());
        assert_eq!(found.get("X"), Some(&1));
        // "y" is not an exact match of "Y": discovered, but the override's Y is emitted first.
        assert_eq!(found.get("y"), Some(&0));
        let other: Combos = [("Z".to_string(), 7), ("X".to_string(), 9)].into();
        assert_eq!(
            defines(&pass, &found, &other),
            "#define Y 5\n#define X 1\n#define Z 7\n"
        );
    }
}
