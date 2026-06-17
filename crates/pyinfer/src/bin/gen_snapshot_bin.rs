//! Build-time generator: snapshot/*.json -> snapshot_bin/*.bin
//!
//! Reuses pyinfer's own JSON `Loader` (via `snapshot::json_to_bin`) so each
//! `.bin` blob is the postcard encoding of the EXACT post-walk `SnapModule`
//! the runtime would otherwise build from the JSON. Run this whenever the JSON
//! source of truth changes (after harness/gen_snapshot.py):
//!
//!     cargo run -p pyinfer --bin gen_snapshot_bin --release
//!
//! Then rebuild prylint so build.rs embeds the refreshed blobs. The .bin files
//! are committed alongside the JSON.

use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let snap_dir = manifest.join("snapshot");
    let bin_dir = manifest.join("snapshot_bin");
    std::fs::create_dir_all(&bin_dir).expect("create snapshot_bin dir");

    let mut names: Vec<String> = std::fs::read_dir(&snap_dir)
        .expect("snapshot dir must exist")
        .filter_map(|e| {
            let name = e.ok()?.file_name().into_string().ok()?;
            let modname = name.strip_suffix(".json")?;
            (modname != "MANIFEST").then(|| modname.to_string())
        })
        .collect();
    names.sort();

    let mut total_json = 0u64;
    let mut total_bin = 0u64;
    let mut failures = Vec::new();

    for n in &names {
        let json_path = snap_dir.join(format!("{n}.json"));
        let json = std::fs::read_to_string(&json_path).expect("read snapshot json");
        total_json += json.len() as u64;
        match pyinfer::snapshot::json_to_bin(&json) {
            Some(bytes) => {
                total_bin += bytes.len() as u64;
                let out = bin_dir.join(format!("{n}.bin"));
                std::fs::write(&out, &bytes).expect("write snapshot bin");
            }
            None => failures.push(n.clone()),
        }
    }

    println!(
        "gen_snapshot_bin: wrote {} blob(s) to {}",
        names.len() - failures.len(),
        bin_dir.display()
    );
    println!(
        "  total JSON {:.2} MB -> total bin {:.2} MB ({:.0}% of JSON)",
        total_json as f64 / 1e6,
        total_bin as f64 / 1e6,
        100.0 * total_bin as f64 / total_json as f64
    );
    if !failures.is_empty() {
        eprintln!("FAILED to load (left without .bin): {:?}", failures);
        std::process::exit(1);
    }
}
