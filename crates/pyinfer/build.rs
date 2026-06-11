//! Embed the astroid snapshot JSONs (snapshot/*.json, ~8.4MB — the
//! post-brain view of builtins + C-extension stdlib modules) into the
//! binary so the released prylint needs no repo checkout. Generates a
//! sorted (modname, json) table of include_str!s; MANIFEST.json is
//! harness metadata and is skipped.

use std::fmt::Write as _;
use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let snap_dir = manifest.join("snapshot");
    println!("cargo:rerun-if-changed={}", snap_dir.display());

    let mut names: Vec<String> = std::fs::read_dir(&snap_dir)
        .expect("crates/pyinfer/snapshot must exist (see harness/gen_snapshot.py)")
        .filter_map(|e| {
            let name = e.ok()?.file_name().into_string().ok()?;
            let modname = name.strip_suffix(".json")?;
            (modname != "MANIFEST").then(|| modname.to_string())
        })
        .collect();
    names.sort();

    let mut out = String::new();
    out.push_str(
        "/// (modname, json) pairs, sorted by modname for binary_search.\n\
         pub static SNAPSHOTS: &[(&str, &str)] = &[\n",
    );
    for n in &names {
        let path = snap_dir.join(format!("{n}.json"));
        writeln!(out, "    ({n:?}, include_str!({:?})),", path.display()).unwrap();
    }
    out.push_str("];\n");

    let dest = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("snapshot_data.rs");
    std::fs::write(dest, out).unwrap();
}
