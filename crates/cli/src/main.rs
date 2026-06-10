mod discover;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut paths: Vec<String> = Vec::new();
    let mut dump_fileitems = false;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "-E" | "--errors-only" => {}
            "--dump-fileitems" => dump_fileitems = true,
            _ if a.starts_with("--disable=") => {}
            _ if a.starts_with("--rcfile=") => {}
            "--disable" | "--rcfile" => i += 1,
            _ if a.starts_with('-') => {
                eprintln!("prylint: unsupported option: {a}");
                return ExitCode::from(32);
            }
            _ => paths.push(a.clone()),
        }
        i += 1;
    }

    let cfg = discover::DiscoverConfig::default();
    let items = discover::expand_modules_fs(&paths, &cfg);

    if dump_fileitems {
        for it in &items {
            println!(
                "{}",
                serde_json::json!({"name": it.name, "path": it.filepath})
            );
        }
        return ExitCode::SUCCESS;
    }

    ExitCode::SUCCESS
}
