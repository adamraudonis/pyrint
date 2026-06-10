mod discover;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut paths: Vec<String> = Vec::new();
    let mut dump_fileitems = false;
    let mut dump_ast = false;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "-E" | "--errors-only" => {}
            "--dump-fileitems" => dump_fileitems = true,
            "--dump-ast" => dump_ast = true,
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

    if dump_ast {
        for path in &paths {
            println!("=== {path}");
            let bytes = match std::fs::read(path) {
                Ok(b) => b,
                Err(e) => {
                    println!("READERROR {e}");
                    continue;
                }
            };
            let text = String::from_utf8_lossy(&bytes).into_owned();
            let src = pyast::SourceFile::from_text(text, "utf-8".to_string());
            match pyast::parse::parse_module(&src, "mod", path, false) {
                pyast::parse::ParseOutcome { tree: Some(tree), .. } => {
                    print!("{}", tree.dump());
                }
                pyast::parse::ParseOutcome { error: Some(e), .. } => {
                    println!("SYNTAXERROR {}:{} {}", e.line, e.offset, e.message);
                }
                _ => unreachable!(),
            }
        }
        return ExitCode::SUCCESS;
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
