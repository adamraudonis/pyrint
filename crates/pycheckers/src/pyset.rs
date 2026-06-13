//! CPython `set` iteration-order simulation under PYTHONHASHSEED=0.
//!
//! pylint exposes raw Python set iteration order in its output in (at least)
//! one place: typecheck.py:1487-1488 iterates
//! `CallSite.duplicated_keywords` (astroid arguments.py:43, a `set[str]`)
//! when emitting E1132 repeated-keyword. Under hash randomization that order
//! is nondeterministic run-to-run (probe-verified), so the pinned ground
//! truth is generated with PYTHONHASHSEED=0 (harness/ground_truth.sh) and
//! this module replicates CPython 3.12's seed-0 behavior exactly:
//!
//! - string hash: `_Py_HashBytes` = siphash13 with a zeroed `_Py_HashSecret`
//!   over the PEP 393 representation (1/2/4 bytes per char, little-endian);
//!   empty string -> 0; result -1 -> -2 (pyhash.c).
//! - set table: open addressing with LINEAR_PROBES=9, PERTURB_SHIFT=5,
//!   PySet_MINSIZE=8, growth at fill*5 >= mask*3 to used*4 (>50000: used*2),
//!   resize re-inserts in old-table slot order via set_insert_clean
//!   (setobject.c). Iteration yields entries in table slot order.
//!
//! No deletions ever happen in the modeled sets, so dummy entries are not
//! modeled.

/// siphash13 with k0 = k1 = 0 (PYTHONHASHSEED=0 zeroes _Py_HashSecret),
/// exactly CPython pyhash.c::pysiphash13.
fn siphash13_zero_key(src: &[u8]) -> u64 {
    let mut b: u64 = (src.len() as u64) << 56;
    let mut v0: u64 = 0x736f6d6570736575;
    let mut v1: u64 = 0x646f72616e646f6d;
    let mut v2: u64 = 0x6c7967656e657261;
    let mut v3: u64 = 0x7465646279746573;

    macro_rules! single_round {
        () => {
            // HALF_ROUND(v0,v1,v2,v3,13,16)
            v0 = v0.wrapping_add(v1);
            v2 = v2.wrapping_add(v3);
            v1 = v1.rotate_left(13) ^ v0;
            v3 = v3.rotate_left(16) ^ v2;
            v0 = v0.rotate_left(32);
            // HALF_ROUND(v2,v1,v0,v3,17,21)
            v2 = v2.wrapping_add(v1);
            v0 = v0.wrapping_add(v3);
            v1 = v1.rotate_left(17) ^ v2;
            v3 = v3.rotate_left(21) ^ v0;
            v2 = v2.rotate_left(32);
        };
    }

    let mut chunks = src.chunks_exact(8);
    for chunk in &mut chunks {
        let mi = u64::from_le_bytes(chunk.try_into().unwrap());
        v3 ^= mi;
        single_round!();
        v0 ^= mi;
    }
    let rem = chunks.remainder();
    let mut t = [0u8; 8];
    t[..rem.len()].copy_from_slice(rem);
    b |= u64::from_le_bytes(t);

    v3 ^= b;
    single_round!();
    v0 ^= b;
    v2 ^= 0xff;
    single_round!();
    single_round!();
    single_round!();

    (v0 ^ v1) ^ (v2 ^ v3)
}

/// CPython `str.__hash__` under PYTHONHASHSEED=0: _Py_HashBytes over the
/// canonical PEP 393 buffer (kind = 1/2/4 bytes per code point, LE).
pub fn pyhash_str_seed0(s: &str) -> i64 {
    if s.is_empty() {
        return 0;
    }
    let max = s.chars().map(|c| c as u32).max().unwrap();
    let bytes: Vec<u8> = if max < 0x100 {
        s.chars().map(|c| c as u32 as u8).collect()
    } else if max < 0x10000 {
        s.chars()
            .flat_map(|c| (c as u32 as u16).to_le_bytes())
            .collect()
    } else {
        s.chars().flat_map(|c| (c as u32).to_le_bytes()).collect()
    };
    let h = siphash13_zero_key(&bytes) as i64;
    if h == -1 {
        -2
    } else {
        h
    }
}

/// CPython 3.12 `tuplehash` (Objects/tupleobject.c) over a slice of element
/// hashes — the xxHash-derived algorithm. Used by symilar: `LinesChunk._hash`
/// is `hash(tuple_of_line_strings)` (the `*succ_lines` in hash_lineset
/// captures the enumerate window as a SINGLE tuple arg, so the chunk hash is
/// the ORDER-SENSITIVE tuple hash, not the sum of line hashes — notes spec
/// B.4 mis-described this). Element hashes are the CPython str hashes
/// (pyhash_str_seed0); PYTHONHASHSEED=0 doesn't affect tuple hashing (the
/// xxprime constants are fixed).
pub fn pyhash_tuple_i64(elems: &[i64]) -> i64 {
    const XXPRIME_1: u64 = 11400714785074694791;
    const XXPRIME_2: u64 = 14029467366897019727;
    const XXPRIME_5: u64 = 2870177450012600261;
    let xxrotate = |x: u64| (x << 31) | (x >> 33);
    let mut acc: u64 = XXPRIME_5;
    for &e in elems {
        let lane = e as u64; // Py_uhash_t cast of the element hash
        acc = acc.wrapping_add(lane.wrapping_mul(XXPRIME_2));
        acc = xxrotate(acc);
        acc = acc.wrapping_mul(XXPRIME_1);
    }
    acc = acc.wrapping_add((elems.len() as u64) ^ (XXPRIME_5 ^ 3527539));
    if acc == u64::MAX {
        // (Py_uhash_t)-1 -> 1546275796
        return 1546275796;
    }
    acc as i64
}

const LINEAR_PROBES: usize = 9;
const PERTURB_SHIFT: u32 = 5;
const PYSET_MINSIZE: usize = 8;

/// setobject.c::set_insert_clean — used during resize; keys are known
/// distinct, slots never dummy.
fn insert_clean(table: &mut [Option<(usize, i64)>], mask: usize, idx: usize, hash: i64) {
    let mut perturb = hash as u64;
    let mut i = (hash as u64 as usize) & mask;
    loop {
        if table[i].is_none() {
            table[i] = Some((idx, hash));
            return;
        }
        if i + LINEAR_PROBES <= mask {
            for j in 1..=LINEAR_PROBES {
                if table[i + j].is_none() {
                    table[i + j] = Some((idx, hash));
                    return;
                }
            }
        }
        perturb >>= PERTURB_SHIFT;
        i = i
            .wrapping_mul(5)
            .wrapping_add(1)
            .wrapping_add(perturb as usize)
            & mask;
    }
}

/// setobject.c::set_table_resize. Re-inserts live entries in old-table slot
/// order.
fn table_resize(table: &mut Vec<Option<(usize, i64)>>, minused: usize) {
    let mut newsize = PYSET_MINSIZE;
    while newsize <= minused {
        newsize <<= 1;
    }
    let newmask = newsize - 1;
    let old = std::mem::replace(table, vec![None; newsize]);
    for e in old {
        if let Some((idx, h)) = e {
            insert_clean(table, newmask, idx, h);
        }
    }
}

/// Simulate `s = set(); for k in keys: s.add(k); list(s)` on CPython 3.12
/// with PYTHONHASHSEED=0. `keys` must be distinct (re-adding an existing
/// element is a no-op in CPython, so callers dedup keeping first adds).
/// Returns the indices into `keys` in set iteration order.
pub fn cpython_set_order(keys: &[&str]) -> Vec<usize> {
    let mut table: Vec<Option<(usize, i64)>> = vec![None; PYSET_MINSIZE];
    let mut fill: usize = 0;

    for (idx, key) in keys.iter().enumerate() {
        let hash = pyhash_str_seed0(key);
        // setobject.c::set_add_entry (no dummies -> no freeslot handling)
        let mask = table.len() - 1;
        let mut perturb = hash as u64;
        let mut i = (hash as u64 as usize) & mask;
        'probe: loop {
            let probes = if i + LINEAR_PROBES <= mask {
                LINEAR_PROBES
            } else {
                0
            };
            for j in i..=i + probes {
                match table[j] {
                    None => {
                        // found_unused
                        table[j] = Some((idx, hash));
                        fill += 1;
                        if fill * 5 >= mask * 3 {
                            let used = fill;
                            table_resize(
                                &mut table,
                                if used > 50000 { used * 2 } else { used * 4 },
                            );
                        }
                        break 'probe;
                    }
                    Some((eidx, eh)) => {
                        if eh == hash && keys[eidx] == *key {
                            break 'probe; // already present
                        }
                    }
                }
            }
            perturb >>= PERTURB_SHIFT;
            i = i
                .wrapping_mul(5)
                .wrapping_add(1)
                .wrapping_add(perturb as usize)
                & mask;
        }
    }

    table.into_iter().flatten().map(|(idx, _)| idx).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn str_hashes_match_cpython_seed0() {
        // values from .venv-pylint python with PYTHONHASHSEED=0
        assert_eq!(pyhash_str_seed0("binary"), 3715517466192343896);
        assert_eq!(pyhash_str_seed0("availability_zone"), -4949992296790722957);
        assert_eq!(pyhash_str_seed0("created_at"), -6795635030330127729);
        assert_eq!(pyhash_str_seed0("updated_at"), 7263017014536680292);
        assert_eq!(pyhash_str_seed0("host"), 6774303931659936088);
        assert_eq!(pyhash_str_seed0("disabled"), 7508752880049457006);
        assert_eq!(pyhash_str_seed0(""), 0);
    }

    #[test]
    fn set_order_matches_cpython_seed0() {
        // PYTHONHASHSEED=0 python: list(set) after adding in this order ==
        // ['host', 'updated_at', 'disabled', 'created_at',
        //  'availability_zone', 'binary']  (6 elems -> resize to 32)
        let keys = [
            "binary",
            "availability_zone",
            "created_at",
            "updated_at",
            "host",
            "disabled",
        ];
        let order: Vec<&str> = cpython_set_order(&keys).into_iter().map(|i| keys[i]).collect();
        assert_eq!(
            order,
            ["host", "updated_at", "disabled", "created_at", "availability_zone", "binary"]
        );
    }
}

#[cfg(test)]
mod tuple_hash_tests {
    use super::*;
    #[test]
    fn tuple_hash_matches_cpython_seed0() {
        let lines = ["}", "static", "char *key = \"__f2py_cb_", "return ("];
        let hs: Vec<i64> = lines.iter().map(|s| pyhash_str_seed0(s)).collect();
        assert_eq!(pyhash_tuple_i64(&hs), 1707547713546452791);
        // reordered -> different (order-sensitive)
        let w18 = ["static", "char *key = \"__f2py_cb_", "return (", "}"];
        let hs18: Vec<i64> = w18.iter().map(|s| pyhash_str_seed0(s)).collect();
        assert_eq!(pyhash_tuple_i64(&hs18), -3792152961302195129);
    }
}
