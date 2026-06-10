//! Port of pylint's file expansion for `pylint <args>` (recursive=False path):
//! pylint/lint/expand_modules.py `expand_modules` +
//! astroid/modutils.py `get_module_files`, `modpath_from_file_with_callback`.
//!
//! Behavioral notes (verified empirically against pylint 4.0.5):
//! - `pylint .` where `.` has no `__init__.py` -> namespace mode: every
//!   directory is walked (including hidden ones, excluding "CVS"), all
//!   `.py/.pyi/.so/.pyd/.pyw` collected, then non-(py|pyi) filtered by
//!   should_analyze_file.
//! - File order = os.walk order = readdir order, files of a directory
//!   before its subdirectories' files; symlinked dirs are not descended.
//! - Module names are dotted relative paths from the matched search path,
//!   `__init__` suffix KEPT in the FileItem name (astroid strips it later
//!   for node-based messages).

use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use indexmap::IndexMap;
use regex::Regex;

#[derive(Debug, Clone)]
pub struct FileItem {
    /// Dotted module name as pylint's FileItem.name (e.g. "pkg.sub.__init__").
    pub name: String,
    /// Path exactly as pylint would carry it (normpath'd, relative if the
    /// argument was relative).
    pub filepath: String,
    /// True if this path was given directly as an argument.
    pub is_arg: bool,
}

pub struct DiscoverConfig {
    /// --ignore (basenames), default ["CVS"]
    pub ignore: Vec<String>,
    /// --ignore-patterns (regexes matched against basenames), default ["^\.#"]
    pub ignore_patterns: Vec<Regex>,
    /// --ignore-paths (regexes matched against normpath'd path), default []
    pub ignore_paths: Vec<Regex>,
}

impl Default for DiscoverConfig {
    fn default() -> Self {
        DiscoverConfig {
            ignore: vec!["CVS".to_string()],
            ignore_patterns: vec![Regex::new(r"^\.#").unwrap()],
            ignore_paths: vec![],
        }
    }
}

/// os.path.normpath for the path strings we handle (pure-lexical, like Python).
pub fn normpath(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let is_abs = path.starts_with('/');
    // Python keeps up to two leading slashes
    let lead_slashes = if path.starts_with("//") && !path.starts_with("///") {
        2
    } else if is_abs {
        1
    } else {
        0
    };
    let mut comps: Vec<&str> = Vec::new();
    for comp in path.split('/') {
        if comp.is_empty() || comp == "." {
            continue;
        }
        if comp != ".."
            || (lead_slashes == 0 && comps.is_empty())
            || (!comps.is_empty() && *comps.last().unwrap() == "..")
        {
            comps.push(comp);
        } else if !comps.is_empty() {
            comps.pop();
        }
    }
    let mut out = "/".repeat(lead_slashes);
    out.push_str(&comps.join("/"));
    if out.is_empty() {
        ".".to_string()
    } else {
        out
    }
}

fn basename(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[i + 1..],
        None => path,
    }
}

/// `_is_ignored_file` port.
fn is_ignored_file(cfg: &DiscoverConfig, element: &str) -> bool {
    let element = normpath(element);
    // Path(element).absolute().name -- for our purposes the basename of the
    // normpath'd element (Path.name of an absolute version is the same
    // basename; for "." it becomes the cwd dir name).
    let base = if element == "." {
        // Path('.').absolute().name == basename of cwd
        std::env::current_dir()
            .ok()
            .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()))
            .unwrap_or_default()
    } else {
        basename(&element).to_string()
    };
    if cfg.ignore.iter().any(|i| *i == base) {
        return true;
    }
    if cfg.ignore_patterns.iter().any(|r| r.find(&base).map_or(false, |m| m.start() == 0)) {
        return true;
    }
    cfg.ignore_paths
        .iter()
        .any(|r| r.find(&element).map_or(false, |m| m.start() == 0))
}

fn is_python_file(filename: &str) -> bool {
    filename.ends_with(".py")
        || filename.ends_with(".pyi")
        || filename.ends_with(".so")
        || filename.ends_with(".pyd")
        || filename.ends_with(".pyw")
}

fn has_init(dir: &Path) -> bool {
    for ext in ["py", "pyw", "pyi", "pyc", "pyo"] {
        if dir.join(format!("__init__.{ext}")).exists() {
            return true;
        }
    }
    false
}

/// `discover_package_path` port (no source-roots configured).
fn discover_package_path(modulepath: &str) -> PathBuf {
    let mut dirname = fs::canonicalize(modulepath)
        .unwrap_or_else(|_| PathBuf::from(modulepath));
    if !dirname.is_dir() {
        dirname = dirname.parent().map(Path::to_path_buf).unwrap_or(dirname);
    }
    loop {
        if !dirname.join("__init__.py").exists() {
            return dirname;
        }
        match dirname.parent() {
            Some(p) if p != dirname => dirname = p.to_path_buf(),
            _ => return std::env::current_dir().unwrap_or(dirname),
        }
    }
}

/// `astroid.modutils.get_module_files` port. Returns paths joined with "/"
/// onto `src_directory` in os.walk order.
fn get_module_files(src_directory: &str, blacklist: &[String], list_all: bool) -> Vec<String> {
    let mut files = Vec::new();
    walk(src_directory, blacklist, list_all, &mut files);
    files
}

fn walk(directory: &str, blacklist: &[String], list_all: bool, files: &mut Vec<String>) {
    // `if directory in blacklist: continue` -- full-string match against the
    // current dirpath. (Only prunes this dir's files; subdirs were already
    // removed by the basename check below when we recursed.)
    let dir_blacklisted = blacklist.iter().any(|b| b == directory);

    let entries = match fs::read_dir(directory) {
        Ok(e) => e,
        Err(_) => return, // os.walk onerror=None: skip silently
    };
    let mut dirnames: Vec<String> = Vec::new();
    let mut filenames: Vec<String> = Vec::new();
    let mut symlink_dirs: HashSet<String> = HashSet::new();
    for entry in entries.flatten() {
        let name = match entry.file_name().into_string() {
            Ok(n) => n,
            Err(_) => continue, // non-UTF8 names: pylint would handle as str; skip
        };
        // os.walk classifies via entry.is_dir() which follows symlinks
        let is_dir = entry
            .path()
            .metadata()
            .map(|m| m.is_dir())
            .unwrap_or(false);
        if is_dir {
            let is_link = entry
                .file_type()
                .map(|t| t.is_symlink())
                .unwrap_or(false);
            if is_link {
                symlink_dirs.insert(name.clone());
            }
            dirnames.push(name);
        } else {
            filenames.push(name);
        }
    }
    // _handle_blacklist: remove blacklisted entries from both lists
    dirnames.retain(|d| !blacklist.iter().any(|b| b == d));
    filenames.retain(|f| !blacklist.iter().any(|b| b == f));

    // check for __init__.py / __init__.pyi
    if !list_all
        && !filenames.iter().any(|f| f == "__init__.py" || f == "__init__.pyi")
    {
        return;
    }

    if !dir_blacklisted {
        for filename in &filenames {
            if is_python_file(filename) {
                files.push(join(directory, filename));
            }
        }
    }

    for d in &dirnames {
        if symlink_dirs.contains(d) {
            continue; // os.walk(followlinks=False)
        }
        walk(&join(directory, d), blacklist, list_all, files);
    }
}

fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// `_get_relative_base_path` port.
fn get_relative_base_path(filename: &str, path_to_check: &str) -> Option<Vec<String>> {
    let path_to_check = normpath(path_to_check); // normcase is identity on mac/linux
    let abs_filename = absolute(filename);
    let from = |f: &str| -> Option<Vec<String>> {
        if !is_subpath(f, &path_to_check) {
            return None;
        }
        let base = strip_ext(f);
        let rel = base[path_to_check.len()..].trim_start_matches('/');
        Some(rel.split('/').filter(|p| !p.is_empty()).map(String::from).collect())
    };
    if let Some(parts) = from(&abs_filename) {
        return Some(parts);
    }
    let real = fs::canonicalize(filename)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| abs_filename.clone());
    from(&real)
}

/// os.path.abspath: join with cwd + normpath, but WITHOUT resolving symlinks
/// in the trailing components. Note Python's getcwd() resolves the cwd itself.
fn absolute(path: &str) -> String {
    if path.starts_with('/') {
        normpath(path)
    } else {
        let cwd = std::env::current_dir().unwrap_or_default();
        normpath(&format!("{}/{}", cwd.to_string_lossy(), path))
    }
}

fn is_subpath(path: &str, base: &str) -> bool {
    // astroid _is_subpath: path == base or path.startswith(base + sep)
    path == base || path.starts_with(&format!("{}/", base.trim_end_matches('/')))
}

fn strip_ext(path: &str) -> &str {
    let base = basename(path);
    match base.rfind('.') {
        Some(i) if i > 0 => &path[..path.len() - (base.len() - i)],
        _ => path,
    }
}

/// `modpath_from_file_with_callback` port.
/// `is_package_cb(pathname, parts)` mirrors the Python callback.
fn modpath_from_file_with_callback(
    filename: &str,
    search_paths: &[String],
    is_package_cb: &dyn Fn(&str, &[String]) -> bool,
) -> Option<Vec<String>> {
    // round 1: paths as given; round 2: realpath'd paths
    let round2: Vec<String> = search_paths
        .iter()
        .map(|p| {
            fs::canonicalize(p)
                .map(|c| c.to_string_lossy().into_owned())
                .unwrap_or_else(|_| p.clone())
        })
        .collect();
    for pathname in search_paths.iter().chain(round2.iter()) {
        if pathname.is_empty() {
            continue;
        }
        if let Some(modpath) = get_relative_base_path(filename, pathname) {
            if modpath.is_empty() {
                continue;
            }
            if is_package_cb(pathname, &modpath[..modpath.len() - 1]) {
                return Some(modpath);
            }
        }
    }
    None
}

fn check_modpath_has_init(path: &str, mod_path: &[String]) -> bool {
    let mut p = PathBuf::from(path);
    for part in mod_path {
        p = p.join(part);
        if !has_init(&p) {
            return false;
        }
    }
    true
}

/// Full `expand_modules` port for filesystem arguments (files/dirs only --
/// module-name arguments like `pylint somemodule` are not supported yet).
/// Returns FileItems in pylint's exact order, post-`should_analyze_file`.
pub fn expand_modules_fs(args: &[String], cfg: &DiscoverConfig) -> Vec<FileItem> {
    // result: IndexMap keyed by normpath(filepath)
    #[derive(Clone)]
    struct Desc {
        name: String,
        path: String,
        is_arg: bool,
        is_ignored: bool,
    }
    let mut result: IndexMap<String, Desc> = IndexMap::new();

    for something in args {
        if is_ignored_file(cfg, something) {
            result.insert(
                something.clone(),
                Desc {
                    name: String::new(),
                    path: something.clone(),
                    is_arg: false,
                    is_ignored: true,
                },
            );
            continue;
        }
        let module_package_path = discover_package_path(something);
        // additional_search_path = ['.', module_package_path, *sys.path]
        let search_paths: Vec<String> = vec![
            ".".to_string(),
            module_package_path.to_string_lossy().into_owned(),
        ];
        let p = Path::new(something);
        let (modname, filepath): (String, String) = if p.exists() {
            let modname = modpath_from_file_with_callback(something, &search_paths, &|path, parts| {
                check_modpath_has_init(path, parts)
            })
            .map(|parts| parts.join("."))
            .unwrap_or_else(|| {
                let b = basename(something);
                strip_ext_name(b).to_string()
            });
            let filepath = if p.is_dir() {
                join(something, "__init__.py")
            } else {
                something.clone()
            };
            (modname, filepath)
        } else {
            // module-name argument: unsupported for now
            continue;
        };
        let filepath = normpath(&filepath);
        let modparts: Vec<String> = {
            let m = if modname.is_empty() { something.clone() } else { modname.clone() };
            m.split('.').map(String::from).collect()
        };
        // file_info_from_modpath: for dir args this raises ImportError when
        // the dir isn't importable as a module name; emulate the two cases.
        let (is_namespace, is_directory) = spec_lookup(&modparts, &filepath, something);

        if !is_namespace {
            let e = result.entry(filepath.clone()).or_insert(Desc {
                name: modname.clone(),
                path: filepath.clone(),
                is_arg: true,
                is_ignored: false,
            });
            e.is_arg = true;
        }
        let has_init_flag = modparts.last().map(|s| s != "__init__").unwrap_or(true)
            && basename(&filepath) == "__init__.py";
        if has_init_flag || is_namespace || is_directory {
            let dir = match filepath.rfind('/') {
                Some(i) => filepath[..i].to_string(),
                None => ".".to_string(),
            };
            for subfilepath in get_module_files(&dir, &cfg.ignore, is_namespace) {
                let subfilepath = normpath(&subfilepath);
                if filepath == subfilepath {
                    continue;
                }
                if is_ignored_file(cfg, &subfilepath) {
                    result.insert(
                        subfilepath.clone(),
                        Desc {
                            name: String::new(),
                            path: subfilepath,
                            is_arg: false,
                            is_ignored: true,
                        },
                    );
                    continue;
                }
                let modpath = modpath_from_file_with_callback(
                    &subfilepath,
                    &search_paths,
                    &|path, parts| is_namespace || check_modpath_has_init(path, parts),
                );
                let submodname = match modpath {
                    Some(parts) => parts.join("."),
                    None => continue, // ImportError would crash pylint; can't happen in practice
                };
                let isarg = result.get(&subfilepath).map(|d| d.is_arg).unwrap_or(false);
                result.insert(
                    subfilepath.clone(),
                    Desc {
                        name: submodname,
                        path: subfilepath,
                        is_arg: isarg,
                        is_ignored: false,
                    },
                );
            }
        }
    }

    // _iterate_file_descrs: skip ignored, apply should_analyze_file
    result
        .into_iter()
        .filter_map(|(_, d)| {
            if d.is_ignored {
                return None;
            }
            if !d.is_arg && !(d.path.ends_with(".py") || d.path.ends_with(".pyi")) {
                return None;
            }
            Some(FileItem {
                name: d.name,
                filepath: d.path,
                is_arg: d.is_arg,
            })
        })
        .collect()
}

fn strip_ext_name(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if i > 0 => &name[..i],
        _ => name,
    }
}

/// Emulate `modutils.file_info_from_modpath` enough for filesystem args:
/// returns (is_namespace, is_directory).
fn spec_lookup(modparts: &[String], filepath: &str, something: &str) -> (bool, bool) {
    // For a directory argument: if dir/__init__.py exists, the spec is found
    // as PKG_DIRECTORY (is_namespace=False, is_directory=True). If not, the
    // spec lookup raises ImportError -> is_namespace = not exists(filepath),
    // is_directory = isdir(something).
    let p = Path::new(something);
    if p.is_dir() {
        if Path::new(filepath).exists() {
            (false, true)
        } else {
            (true, p.is_dir())
        }
    } else {
        // plain file: find_spec on its modparts; a file with __init__ along
        // its package path is found as PY_SOURCE (not namespace). A loose
        // file not on any package path: ImportError -> is_namespace =
        // not exists(filepath) = False (it exists), is_directory = False.
        let _ = modparts;
        (false, false)
    }
}

#[cfg(test)]
mod tests {
    use super::normpath;

    #[test]
    fn test_normpath() {
        assert_eq!(normpath("./a/b.py"), "a/b.py");
        assert_eq!(normpath("a/./b.py"), "a/b.py");
        assert_eq!(normpath("a/../b.py"), "b.py");
        assert_eq!(normpath("."), ".");
        assert_eq!(normpath("/a//b"), "/a/b");
    }
}
