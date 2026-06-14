//! SimilaritiesChecker / Symilar port (pylint/checkers/symilar.py): R0801
//! duplicate-code. A BaseRawFileChecker (process_module per file) + close()
//! at end-of-run computing cross-module similar line blocks.
//!
//! Defaults (the CHECKER defaults, not the standalone CLI): min-similarity-
//! lines=4, ignore-comments=y, ignore-docstrings=y, ignore-imports=y,
//! ignore-signatures=y.
//!
//! Chunk hashing uses the exact CPython str hash (siphash13, PYTHONHASHSEED=0)
//! summed per window (pyset::pyhash_str_seed0) — byte-identical to pylint's
//! `sum(hash(lin) for lin in lines)`. Set/frozenset iteration + intersection
//! order are replicated via the cpython_set_order port.

use pyast::tree::NodeKind;
use pyast::NodeId;
use pyinfer::graph::Engine;
use pyinfer::value::ModId;

use crate::pyset::{pyhash_str_seed0, pyhash_tuple_i64};

const MIN_SIMILARITY_LINES: usize = 4;
const IGNORE_COMMENTS: bool = true;
const IGNORE_DOCSTRINGS: bool = true;
const IGNORE_IMPORTS: bool = true;
const IGNORE_SIGNATURES: bool = true;

/// LineSpecifs: a non-empty stripped line. line_number is 0-based (the real
/// file line minus 1, as stored by stripped_lines).
#[derive(Clone)]
struct LineSpec {
    line_number: usize,
    text: String,
}

/// LineSet: name (module dotted name), real lines (raw, with terminators),
/// stripped lines. __len__ = real_lines.len() (stats denominator).
pub struct LineSet {
    name: String,
    real_lines: Vec<String>,
    stripped: Vec<LineSpec>,
}

/// SimilaritiesChecker state. Lives in LintRun; open() resets per RUN.
#[derive(Default)]
pub struct SimilaritiesCk {
    pub linesets: Vec<LineSet>,
}

impl SimilaritiesCk {
    /// process_module: decode the file's bytes via file_encoding, readlines(),
    /// build a LineSet. `is_line_enabled` is the per-line R0801 pragma gate
    /// (linter._is_one_message_enabled("R0801", lineno) on THIS file's state).
    /// `import_lines`/`signature_lines` are 1-based line sets derived from the
    /// module tree (ignore-imports/ignore-signatures).
    #[allow(clippy::too_many_arguments)]
    pub fn process_module(
        &mut self,
        name: &str,
        real_lines: Vec<String>,
        import_lines: &rustc_hash::FxHashSet<u32>,
        signature_lines: &rustc_hash::FxHashSet<u32>,
        is_line_enabled: &dyn Fn(u32) -> bool,
    ) {
        let stripped = stripped_lines(
            &real_lines,
            import_lines,
            signature_lines,
            is_line_enabled,
        );
        self.linesets.push(LineSet {
            name: name.to_string(),
            real_lines,
            stripped,
        });
    }

    /// close(): compute similarities and emit R0801. `emit` receives the
    /// fully-formatted message body (the caller stamps module/line). Returns
    /// the total `duplicated` increment for stats (num*(len(couples)-1) per
    /// message) and the total real-line count for percent.
    pub fn close(&self, emit: &mut dyn FnMut(usize, String)) -> (i64, i64) {
        let total: i64 = self.linesets.iter().map(|ls| ls.real_lines.len() as i64).sum();
        let mut duplicated: i64 = 0;
        for (num, couples) in self.compute_sims() {
            // msg headers: ==name:[start:end] for each couple, then sorted.
            let headers: Vec<(String, &CoupleRef)> = couples
                .iter()
                .map(|c| {
                    (
                        format!("=={}:[{}:{}]", self.linesets[c.lineset].name, c.start, c.end),
                        c,
                    )
                })
                .collect();
            let mut msg: Vec<String> = headers.iter().map(|(h, _)| h.clone()).collect();
            msg.sort();
            // Real-lines code block — the CHECKER close() (symilar.py:849-855,
            // NOT the standalone CLI's `sorted(couples)` at 442-464):
            //     for lineset, start_line, end_line in couples:   # SET order!
            //         msg.append(f"=={lineset.name}:[{start_line}:{end_line}]")
            //     msg.sort()                                      # headers only
            //     if lineset:
            //         for line in lineset.real_lines[start_line:end_line]: ...
            // The header strings get sorted, but the (lineset, start, end)
            // bindings used to print real lines retain the LAST element of the
            // *set* iteration. `couples` is a Python set of (LineSet, int, int)
            // and LineSet.__hash__ == id(self), so the iteration order — hence
            // which region's real lines are printed — depends on heap
            // addresses. It is UPSTREAM-NONDETERMINISTIC run-to-run (notes/09
            // §B.7: 5 identical invocations flip the printed block). When the
            // two regions' rstripped real lines are identical (true copy-paste,
            // the common case) any choice is byte-identical.
            //
            // EMPIRICAL (salt full GT, 2153 blocks): 807 ambiguous (regions
            // identical → choice irrelevant), 1345 decidable, of which the GT's
            // printed region splits 708 sorts-FIRST-by-name / 637 sorts-LAST
            // (and 684/661 by start) — a ~53/47 coin flip with NO deterministic
            // predictor, confirming the id/heap dependence. We adopt a stable
            // sorts-FIRST policy on (name, start, end) — the single rule that
            // maximizes parity with this GT (1515/2153 = 807+708). The residual
            // 637 mismatches are pylint's own heap nondeterminism, not a
            // prylint defect, and are unreachable without CPython object ids.
            if let Some(chosen) = couples.iter().min_by(|a, b| {
                let an = self.linesets[a.lineset].name.as_str();
                let bn = self.linesets[b.lineset].name.as_str();
                an.cmp(bn)
                    .then(a.start.cmp(&b.start))
                    .then(a.end.cmp(&b.end))
            }) {
                let ls = &self.linesets[chosen.lineset];
                for line in ls.real_lines.get(chosen.start..chosen.end).unwrap_or(&[]) {
                    msg.push(line.trim_end().to_string());
                }
            }
            emit(couples.len(), msg.join("\n"));
            duplicated += (num * (couples.len() - 1)) as i64;
        }
        (duplicated, total)
    }

    /// _compute_sims (symilar.py:398-433): group commonalities by num,
    /// dedup so each duplicated block yields one 2-file pair, sort by
    /// (num, set) reverse=True (ties keep discovery order).
    fn compute_sims(&self) -> Vec<(usize, Vec<CoupleRef>)> {
        // no_duplicates: num -> list of sets (each set = exactly 2 triples).
        let mut no_duplicates: indexmap::IndexMap<usize, Vec<Vec<CoupleRef>>> =
            indexmap::IndexMap::new();
        // Per-num index of every triple already claimed by some set, for O(1)
        // membership. pylint scans all sets in the bucket for either triple
        // and `break`s (drops the pair) on the first hit, never extending the
        // matched set — so a triple, once in any set, blocks all later pairs
        // touching it. Aggregating claimed triples into a set is equivalent
        // and avoids the O(commonalities^2) per-bucket scan (black: minutes).
        let mut claimed: rustc_hash::FxHashMap<usize, rustc_hash::FxHashSet<CoupleRef>> =
            rustc_hash::FxHashMap::default();
        for com in self.iter_sims() {
            let num = com.cmn_lines_nb;
            let pair = [
                CoupleRef { lineset: com.fst_lset, start: com.fst_start, end: com.fst_end },
                CoupleRef { lineset: com.snd_lset, start: com.snd_start, end: com.snd_end },
            ];
            let seen = claimed.entry(num).or_default();
            if seen.contains(&pair[0]) || seen.contains(&pair[1]) {
                continue;
            }
            seen.insert(pair[0]);
            seen.insert(pair[1]);
            no_duplicates.entry(num).or_default().push(vec![pair[0], pair[1]]);
        }
        // sims = [(num, cpls) for num, ensembles in no_duplicates.items()
        //         for cpls in ensembles]; sorted(reverse=True)
        let mut sims: Vec<(usize, Vec<CoupleRef>)> = Vec::new();
        for (num, ensembles) in &no_duplicates {
            for cpls in ensembles {
                sims.push((*num, cpls.clone()));
            }
        }
        // sorted(sims, reverse=True): tuple (int, set) compare. Equal num ->
        // sets compared by == then < (proper-subset) -> distinct 2-element
        // sets are never <, so equal-num entries KEEP insertion order under
        // a stable reverse sort (CPython: ties not reversed). We emulate with
        // a stable sort by descending num only.
        sims.sort_by(|a, b| b.0.cmp(&a.0));
        sims
    }

    /// _iter_sims (symilar.py:542-548): cartesian over i<j lineset pairs.
    /// pylint recomputes hash_lineset PER PAIR; the result is identical each
    /// time (linesets immutable; remove_successive only mutates per-pair COPIES
    /// stored in CplLines), so we precompute once per lineset and reuse —
    /// output-identical, O(n^2*L) instead of O(n^3*L).
    fn iter_sims(&self) -> Vec<Commonality> {
        let mut out = Vec::new();
        let n = self.linesets.len();
        let hashed: Vec<(Hash2Index, Index2Lines)> = self
            .linesets
            .iter()
            .map(|ls| hash_lineset(ls, MIN_SIMILARITY_LINES))
            .collect();
        for i in 0..n.saturating_sub(1) {
            for j in (i + 1)..n {
                self.find_common(i, j, &hashed[i], &hashed[j], &mut out);
            }
        }
        out
    }

    /// _find_common (symilar.py:467-540).
    fn find_common(
        &self,
        idx1: usize,
        idx2: usize,
        hashed1: &(Hash2Index, Index2Lines),
        hashed2: &(Hash2Index, Index2Lines),
        out: &mut Vec<Commonality>,
    ) {
        let ls1 = &self.linesets[idx1];
        let ls2 = &self.linesets[idx2];
        let (h2i_1, i2l_1) = hashed1;
        let (h2i_2, i2l_2) = hashed2;

        // hash_1 = frozenset(h2i_1.keys()); hash_2 = frozenset(h2i_2.keys());
        // common = sorted(hash_1 & hash_2, key=lambda m: h2i_1[m][0]);
        // then re-sorted by representative _index.
        //
        // The intersection representative selection (CPython set_intersection
        // iterates the SMALLER operand, ties -> RIGHT operand) determines
        // which side's _index is used. But since we then re-sort entirely by
        // operator.attrgetter("_index") and h2i_1[c_hash][0] is the index in
        // lineset1 for the product step (which uses h2i_1[c_hash] /
        // h2i_2[c_hash] directly, keyed by hash VALUE), the representative's
        // OWN _index only sets the iteration order. The final all_couples
        // insertion order = ascending representative _index. We compute the
        // common hash set and order it by the representative's first-occurrence
        // index in the operand CPython would iterate.
        let common = self.common_hashes(h2i_1, h2i_2);

        // all_couples: LineSetStartCouple(index_1, index_2) -> CplLines.
        // IndexMap preserves insertion order (== product order over the
        // ordered common hashes), which remove_successive depends on.
        let mut all_couples: indexmap::IndexMap<(usize, usize), CplLines> = indexmap::IndexMap::new();
        for h in &common {
            // itertools.product(h2i_1[h], h2i_2[h]) — both ascending
            for &i1 in &h2i_1[h] {
                for &i2 in &h2i_2[h] {
                    all_couples.insert(
                        (i1, i2),
                        CplLines {
                            first: i2l_1[i1],
                            second: i2l_2[i2],
                            effective: MIN_SIMILARITY_LINES,
                        },
                    );
                }
            }
        }

        remove_successive(&mut all_couples);

        for (&(start_index_1, start_index_2), cmn) in &all_couples {
            let nb_common = cmn.effective;
            let eff = filter_noncode_lines(ls1, start_index_1, ls2, start_index_2, nb_common);
            if eff > MIN_SIMILARITY_LINES {
                out.push(Commonality {
                    cmn_lines_nb: nb_common,
                    fst_lset: idx1,
                    fst_start: cmn.first.0,
                    fst_end: cmn.first.1,
                    snd_lset: idx2,
                    snd_start: cmn.second.0,
                    snd_end: cmn.second.1,
                });
            }
        }
    }

    /// common hashes ordered by representative first-occurrence index.
    ///
    /// CPython `hash_1 & hash_2` keeps elements from the SMALLER operand
    /// (ties: RIGHT = hash_2). `sorted(..., key=h2i_1[m][0])` then a re-sort
    /// by `m._index` (the representative chunk's first-occurrence index in ITS
    /// lineset). The final iteration order is ascending representative
    /// `_index`. The representative's `_index` = first occurrence of that hash
    /// in the operand the intersection iterated. We reproduce that selection.
    fn common_hashes(
        &self,
        h2i_1: &Hash2Index,
        h2i_2: &Hash2Index,
    ) -> Vec<i64> {
        // smaller operand iterated; ties -> hash_2 (the right operand)
        let from_2 = h2i_2.len() <= h2i_1.len();
        // intersection: hashes present in both, with representative _index
        // (first-occurrence index) from the iterated operand.
        let mut common: Vec<(usize, i64)> = Vec::new();
        if from_2 {
            for (h, idxs) in h2i_2 {
                if h2i_1.contains_key(h) {
                    common.push((idxs[0], *h));
                }
            }
        } else {
            for (h, idxs) in h2i_1 {
                if h2i_2.contains_key(h) {
                    common.push((idxs[0], *h));
                }
            }
        }
        // sort by representative _index ascending (stable). The earlier
        // sorted(key=h2i_1[m][0]) is subsumed — _index values are unique
        // within one lineset so this ordering is total.
        common.sort_by_key(|&(idx, _)| idx);
        common.into_iter().map(|(_, h)| h).collect()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct CoupleRef {
    lineset: usize,
    start: usize,
    end: usize,
}

struct Commonality {
    cmn_lines_nb: usize,
    fst_lset: usize,
    fst_start: usize,
    fst_end: usize,
    snd_lset: usize,
    snd_start: usize,
    snd_end: usize,
}

#[derive(Clone, Copy)]
struct CplLines {
    /// (start, end) real line numbers in file 1
    first: (usize, usize),
    second: (usize, usize),
    effective: usize,
}

/// remove_successive (symilar.py:248-288): merge consecutive (i,j)/(i+1,j+1)
/// couples in INSERTION ORDER. The head absorbs the run's ends and grows
/// effective by 1 per absorbed couple; absorbed keys are removed.
fn remove_successive(all_couples: &mut indexmap::IndexMap<(usize, usize), CplLines>) {
    // pylint pops absorbed keys from the live dict (dict.pop is O(1)); our
    // previous IndexMap::shift_remove is O(n) per pop -> O(n^2) on the
    // pathological hash-bucket blowups (black profiling/list_huge: ~88M
    // couples). Replicate the EXACT semantics with O(1) membership and a
    // single order-preserving rebuild at the end:
    //  * iterate keys in insertion order (the snapshot pylint takes),
    //  * a key already absorbed by an earlier head is a no-op (its chain was
    //    popped, so `while test in all_couples` never fires),
    //  * a live head walks its (i+1,j+1) chain, absorbing each live successor
    //    (ends + effective) and marking it removed,
    //  * removed keys are dropped from the final map preserving relative order.
    let keys: Vec<(usize, usize)> = all_couples.keys().copied().collect();
    let mut removed: rustc_hash::FxHashSet<(usize, usize)> =
        rustc_hash::FxHashSet::default();
    for couple in &keys {
        if removed.contains(couple) {
            continue;
        }
        let mut test = (couple.0 + 1, couple.1 + 1);
        loop {
            // live == present in the original map AND not yet absorbed.
            if removed.contains(&test) {
                break;
            }
            let Some(succ) = all_couples.get(&test).copied() else {
                break;
            };
            let head = all_couples.get_mut(couple).unwrap();
            head.first.1 = succ.first.1;
            head.second.1 = succ.second.1;
            head.effective += 1;
            removed.insert(test);
            test = (test.0 + 1, test.1 + 1);
        }
    }
    if !removed.is_empty() {
        all_couples.retain(|k, _| !removed.contains(k));
    }
}

/// filter_noncode_lines (symilar.py:291-322): keep only lines matching
/// REGEX_FOR_LINES_WITH_CONTENT = r".*\w+" (contains a word char) in each
/// side's stripped slice, then count positional equality after filtering.
fn filter_noncode_lines(
    ls1: &LineSet,
    st1: usize,
    ls2: &LineSet,
    st2: usize,
    n: usize,
) -> usize {
    let slice1: Vec<&str> = ls1.stripped[st1..(st1 + n).min(ls1.stripped.len())]
        .iter()
        .filter(|l| has_word_char(&l.text))
        .map(|l| l.text.as_str())
        .collect();
    let slice2: Vec<&str> = ls2.stripped[st2..(st2 + n).min(ls2.stripped.len())]
        .iter()
        .filter(|l| has_word_char(&l.text))
        .map(|l| l.text.as_str())
        .collect();
    slice1
        .iter()
        .zip(slice2.iter())
        .filter(|(a, b)| a == b)
        .count()
}

/// re.compile(r".*\w+").match — anchored at start, any prefix then a Python
/// `\w` (word char: alphanumeric + underscore, Unicode by default). The
/// `.*` is greedy but `.match` just needs SOME word char to exist anywhere
/// after the start (since .* can consume up to it). On a line with NO `\w`
/// char it fails. (`.` does not match `\n` but lines are already split.)
fn has_word_char(s: &str) -> bool {
    s.chars().any(|c| c.is_alphanumeric() || c == '_')
}

/// hash_lineset (symilar.py:207-245): sliding windows of `m` stripped lines.
/// Returns (hash -> [start indices], start index -> (real_start, real_end)).
/// Hash key is CPython's ORDER-SENSITIVE tuple hash over the window's m line
/// strings (`LinesChunk._hash = hash(tuple(succ_lines))` — the `*succ_lines`
/// captures the enumerate window as one tuple, so the chunk hash is the tuple
/// hash, NOT the sum of line hashes; the spec's B.4 "sum" description is
/// wrong). pyhash_tuple_i64 over the per-line CPython str hashes.
type Hash2Index = indexmap::IndexMap<i64, Vec<usize>>;
type Index2Lines = Vec<(usize, usize)>;

fn hash_lineset(lineset: &LineSet, m: usize) -> (Hash2Index, Index2Lines) {
    let mut hash2index: Hash2Index = indexmap::IndexMap::new();
    let stripped = &lineset.stripped;
    let n = stripped.len();
    // index2lines indexed by start index i; only valid for i in 0..num_windows
    let mut index2lines: Index2Lines = Vec::new();
    if n < m {
        return (hash2index, index2lines);
    }
    let num_windows = n - m + 1;
    // precompute per-line CPython str hashes (siphash13, PYTHONHASHSEED=0)
    let line_hashes: Vec<i64> = stripped.iter().map(|l| pyhash_str_seed0(&l.text)).collect();
    for i in 0..num_windows {
        let start_linenumber = stripped[i].line_number;
        // end = stripped[i+m].line_number, except IndexError ->
        // stripped[-1].line_number + 1
        let end_linenumber = if i + m < n {
            stripped[i + m].line_number
        } else {
            stripped[n - 1].line_number + 1
        };
        index2lines.push((start_linenumber, end_linenumber));
        // chunk hash = CPython tuple hash of the m window line strings
        let h = pyhash_tuple_i64(&line_hashes[i..i + m]);
        hash2index.entry(h).or_default().push(i);
    }
    (hash2index, index2lines)
}

/// stripped_lines (symilar.py:566-657): produce 0-based-numbered non-empty
/// stripped lines after pragma-drop, strip, docstring machine, comment split,
/// import/signature blanking.
fn stripped_lines(
    lines: &[String],
    import_lines: &rustc_hash::FxHashSet<u32>,
    signature_lines: &rustc_hash::FxHashSet<u32>,
    is_line_enabled: &dyn Fn(u32) -> bool,
) -> Vec<LineSpec> {
    let mut out = Vec::new();
    // ignore_lines = import_lines | signature_lines (both 1-based)
    let mut docstring: Option<String> = None;
    for (idx, raw) in lines.iter().enumerate() {
        let lineno = (idx + 1) as u32; // 1-based
        // 1. pragma callback
        if IGNORE_DOCSTRINGS || IGNORE_COMMENTS || IGNORE_IMPORTS || IGNORE_SIGNATURES {
            // callback is always present (we have a linter)
        }
        if !is_line_enabled(lineno) {
            continue;
        }
        // 2. line = line.strip() (Python str.strip: leading/trailing
        // whitespace incl. the line terminator).
        let mut line = raw.trim().to_string();
        // 3. ignore_docstrings state machine (bug-compatible).
        if IGNORE_DOCSTRINGS {
            if docstring.is_none() {
                if line.starts_with("\"\"\"") || line.starts_with("'''") {
                    docstring = Some(line[..3].to_string());
                    line = line[3..].to_string();
                } else if line.starts_with("r\"\"\"") || line.starts_with("r'''") {
                    docstring = Some(line[1..4].to_string());
                    line = line[4..].to_string();
                }
            }
            if let Some(ds) = &docstring {
                if line.ends_with(ds.as_str()) {
                    docstring = None;
                }
                line = String::new();
            }
        }
        // 4. ignore_comments: line = line.split("#", 1)[0].strip()
        if IGNORE_COMMENTS {
            let cut = line.split_once('#').map(|(a, _)| a).unwrap_or(&line);
            line = cut.trim().to_string();
        }
        // 5. import/signature blanking
        if IGNORE_IMPORTS && import_lines.contains(&lineno) {
            line = String::new();
        }
        if IGNORE_SIGNATURES && signature_lines.contains(&lineno) {
            line = String::new();
        }
        // 6. keep non-empty, store 0-based line number
        if !line.is_empty() {
            out.push(LineSpec {
                line_number: (lineno - 1) as usize,
                text: line,
            });
        }
    }
    out
}

/// Compute the 1-based import-line set (ignore-imports) for a module tree:
/// every Import/ImportFrom node anywhere -> range(lineno, (end_lineno or
/// lineno)+1). Uses fromlineno/end_lineno from the parsed tree (a fresh parse
/// of the same source yields identical line numbers).
pub fn import_lines_of(eng: &Engine, mid: ModId) -> rustc_hash::FxHashSet<u32> {
    let mut set = rustc_hash::FxHashSet::default();
    let md = eng.md(mid);
    for (i, node) in md.tree.nodes.iter().enumerate() {
        if matches!(node.kind, NodeKind::Import { .. } | NodeKind::ImportFrom { .. }) {
            let lo = node.fromlineno;
            let hi = if node.end_lineno == 0 { lo } else { node.end_lineno };
            for l in lo..=hi {
                set.insert(l);
            }
            let _ = i;
        }
    }
    set
}

/// Compute the 1-based signature-line set (ignore-signatures): functions
/// collected by _get_functions (recursing ONLY into Module/ClassDef/
/// FunctionDef/AsyncFunctionDef bodies — NOT if/try/with). For each:
/// range(func.lineno, func.body[0].lineno if func.body else func.tolineno+1).
pub fn signature_lines_of(eng: &Engine, mid: ModId) -> rustc_hash::FxHashSet<u32> {
    let md = eng.md(mid);
    let mut funcs: Vec<NodeId> = Vec::new();
    // start from Module body
    let mut stack: Vec<Vec<NodeId>> = Vec::new();
    if let NodeKind::Module(m) = &md.tree.nodes[NodeId::MODULE.idx()].kind {
        stack.push(m.body.clone());
    }
    while let Some(body) = stack.pop() {
        for n in body {
            match &md.tree.nodes[n.idx()].kind {
                NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => {
                    funcs.push(n);
                    stack.push(d.body.clone());
                }
                NodeKind::ClassDef(d) => {
                    stack.push(d.body.clone());
                }
                _ => {}
            }
        }
    }
    let mut set = rustc_hash::FxHashSet::default();
    for f in funcs {
        let node = &md.tree.nodes[f.idx()];
        let (body0, decorators) = match &node.kind {
            NodeKind::FunctionDef(d) | NodeKind::AsyncFunctionDef(d) => {
                (d.body.first().copied(), d.decorators)
            }
            _ => (None, None),
        };
        // func.lineno: astroid resets a decorated function's lineno to the
        // FIRST decorator's line (rebuilder.py:1130-1139). The Decorators
        // node's fromlineno IS the first decorator line. Undecorated ->
        // func.fromlineno (the def line).
        let lo = match decorators {
            Some(dec) => md.tree.nodes[dec.idx()].fromlineno,
            None => node.fromlineno,
        };
        let hi = match body0 {
            Some(b) => md.tree.nodes[b.idx()].fromlineno,
            None => node.tolineno + 1,
        };
        // range(lo, hi): lo..hi (hi exclusive)
        for l in lo..hi {
            set.insert(l);
        }
    }
    set
}

/// Split decoded text into readlines()-style lines (split on \n, \r\n, \r;
/// keepends). The text comes from decode_source_raw (non-universal newlines,
/// BOM stripped for utf-8-sig).
pub fn readlines(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                out.push(text[start..=i].to_string());
                i += 1;
                start = i;
            }
            b'\r' => {
                // \r\n -> one line ending at \n; lone \r -> line ends at \r
                if i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                    out.push(text[start..=i + 1].to_string());
                    i += 2;
                } else {
                    out.push(text[start..=i].to_string());
                    i += 1;
                }
                start = i;
            }
            _ => i += 1,
        }
    }
    if start < bytes.len() {
        out.push(text[start..].to_string());
    }
    out
}
