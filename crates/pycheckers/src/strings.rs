//! StringFormatChecker (checkers/strings.py) — E1300-E1307/E1310 + the
//! W1300-W1310 computations (disabled; pragma-resurrection + I0021).
//!
//! Includes exact ports of utils.parse_format_string (old-style `%`) and
//! utils.parse_format_method_string / collect_string_fields /
//! split_format_field_names (PEP 3101) per CPython 3.12 semantics.

use pyast::tree::{ConstValue, NodeKind};
use pyast::NodeId;
use pyinfer::ctx::{CallCtx, Ctx};
use pyinfer::graph::Engine;
use pyinfer::value::{ErrKind, GNode, GSym, Value, NV};
use std::cell::RefCell;

use crate::ckutils as u;
use crate::walker::WalkCx;

// ---------------------------------------------------------------------------
// parse_format_string (utils.py:518-591)
// ---------------------------------------------------------------------------

pub enum PctError {
    Incomplete,
    Unsupported(usize), // index of offending char
}

pub struct PctParsed {
    pub keys: indexmap::IndexSet<String>,
    pub num_args: usize,
    pub key_types: std::collections::HashMap<String, char>,
    pub pos_types: Vec<char>,
}

pub fn parse_format_string(s: &str) -> Result<PctParsed, PctError> {
    let chars: Vec<char> = s.chars().collect();
    let mut keys = indexmap::IndexSet::new();
    let mut key_types = std::collections::HashMap::new();
    let mut pos_types = Vec::new();
    let mut num_args = 0usize;
    let next_char = |i: usize| -> Result<(usize, char), PctError> {
        let i = i + 1;
        if i == chars.len() {
            return Err(PctError::Incomplete);
        }
        Ok((i, chars[i]))
    };
    let mut i = 0usize;
    while i < chars.len() {
        let mut char_ = chars[i];
        if char_ == '%' {
            let (ni, nc) = next_char(i)?;
            i = ni;
            char_ = nc;
            let mut key: Option<String> = None;
            if char_ == '(' {
                let mut depth = 1;
                let (ni, nc) = next_char(i)?;
                i = ni;
                char_ = nc;
                let key_start = i;
                while depth != 0 {
                    if char_ == '(' {
                        depth += 1;
                    } else if char_ == ')' {
                        depth -= 1;
                    }
                    let (ni, nc) = next_char(i)?;
                    i = ni;
                    char_ = nc;
                }
                let key_end = i - 1;
                key = Some(chars[key_start..key_end].iter().collect());
            }
            while "#0- +".contains(char_) {
                let (ni, nc) = next_char(i)?;
                i = ni;
                char_ = nc;
            }
            if char_ == '*' {
                num_args += 1;
                let (ni, nc) = next_char(i)?;
                i = ni;
                char_ = nc;
            } else {
                while char_.is_ascii_digit() {
                    let (ni, nc) = next_char(i)?;
                    i = ni;
                    char_ = nc;
                }
            }
            if char_ == '.' {
                let (ni, nc) = next_char(i)?;
                i = ni;
                char_ = nc;
                if char_ == '*' {
                    num_args += 1;
                    let (ni, nc) = next_char(i)?;
                    i = ni;
                    char_ = nc;
                } else {
                    while char_.is_ascii_digit() {
                        let (ni, nc) = next_char(i)?;
                        i = ni;
                        char_ = nc;
                    }
                }
            }
            if "hlL".contains(char_) {
                let (ni, nc) = next_char(i)?;
                i = ni;
                char_ = nc;
            }
            if !"diouxXeEfFgGcrs%a".contains(char_) {
                return Err(PctError::Unsupported(i));
            }
            if let Some(k) = key {
                keys.insert(k.clone());
                key_types.insert(k, char_);
            } else if char_ != '%' {
                num_args += 1;
                pos_types.push(char_);
            }
        }
        i += 1;
    }
    Ok(PctParsed { keys, num_args, key_types, pos_types })
}

// ---------------------------------------------------------------------------
// string.Formatter().parse port (CPython MarkupIterator) + field-name split
// ---------------------------------------------------------------------------

/// One parse entry: (literal, field_name, format_spec, conversion)
pub struct ParseEntry {
    pub field_name: Option<String>,
    pub format_spec: Option<String>,
    pub conversion: Option<char>,
}

/// Err(()) = ValueError (any form) -> IncompleteFormatString upstream
pub fn formatter_parse(s: &str) -> Result<Vec<ParseEntry>, ()> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    let n = chars.len();
    while i < n {
        // literal scan
        let mut markup_follows = false;
        while i < n {
            let c = chars[i];
            if c == '{' || c == '}' {
                if i + 1 < n && chars[i + 1] == c {
                    // doubled brace: literal includes one, entry has no field
                    i += 2;
                    out.push(ParseEntry { field_name: None, format_spec: None, conversion: None });
                    markup_follows = false;
                    break;
                }
                if c == '}' {
                    return Err(()); // Single '}' encountered
                }
                markup_follows = true;
                i += 1; // consume '{'
                break;
            }
            i += 1;
        }
        if !markup_follows {
            if i >= n {
                break;
            }
            continue;
        }
        // field name until '!' ':' '}'
        let start = i;
        while i < n && chars[i] != '!' && chars[i] != ':' && chars[i] != '}' && chars[i] != '{' {
            i += 1;
        }
        if i >= n {
            return Err(()); // Single '{' encountered
        }
        if chars[i] == '{' {
            return Err(()); // unexpected '{' in field name
        }
        let field_name: String = chars[start..i].iter().collect();
        let mut conversion: Option<char> = None;
        if chars[i] == '!' {
            i += 1;
            if i >= n {
                return Err(());
            }
            conversion = Some(chars[i]);
            i += 1;
            if i >= n || (chars[i] != ':' && chars[i] != '}') {
                return Err(());
            }
        }
        let mut format_spec = String::new();
        if i < n && chars[i] == ':' {
            i += 1;
            let mut depth = 1i32;
            let spec_start = i;
            loop {
                if i >= n {
                    return Err(()); // unmatched '{' in format spec
                }
                let c = chars[i];
                if c == '{' {
                    depth += 1;
                } else if c == '}' {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                i += 1;
            }
            format_spec = chars[spec_start..i].iter().collect();
        }
        if i >= n || chars[i] != '}' {
            return Err(());
        }
        i += 1; // consume closing '}'
        out.push(ParseEntry {
            field_name: Some(field_name),
            format_spec: Some(format_spec),
            conversion,
        });
    }
    Ok(out)
}

/// collect_string_fields (utils.py:603-634): yields field names (None
/// entries skipped) recursing into nested format specs.
pub fn collect_string_fields(s: &str, out: &mut Vec<String>) -> Result<(), ()> {
    let entries = formatter_parse(s)?;
    for e in entries {
        if e.field_name.is_none() && e.format_spec.is_none() && e.conversion.is_none() {
            continue;
        }
        // `if all(item is None for item in result[1:])` — a doubled-brace
        // entry has all None -> skipped; any real field yields name
        let Some(name) = e.field_name else { continue };
        out.push(name);
        if let Some(spec) = e.format_spec {
            if !spec.is_empty() {
                collect_string_fields(&spec, out)?;
            }
        }
    }
    Ok(())
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum KeyName {
    Num(i64),
    Str(String),
}

#[derive(Clone, Debug)]
pub enum FieldSpec {
    Attr(String),
    IndexNum(i64),
    IndexStr(String),
}

/// _string.formatter_field_name_split port. Err(()) -> IncompleteFormatString
pub fn split_format_field_names(s: &str) -> Result<(KeyName, Vec<FieldSpec>), ()> {
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    let mut i = 0usize;
    while i < n && chars[i] != '.' && chars[i] != '[' {
        i += 1;
    }
    let first: String = chars[..i].iter().collect();
    let keyname = if !first.is_empty() && first.chars().all(|c| c.is_ascii_digit()) {
        KeyName::Num(first.parse().map_err(|_| ())?)
    } else {
        KeyName::Str(first)
    };
    let mut specs = Vec::new();
    while i < n {
        match chars[i] {
            '.' => {
                i += 1;
                let start = i;
                while i < n && chars[i] != '.' && chars[i] != '[' {
                    i += 1;
                }
                if start == i {
                    return Err(()); // Empty attribute in format string
                }
                specs.push(FieldSpec::Attr(chars[start..i].iter().collect()));
            }
            '[' => {
                i += 1;
                let start = i;
                while i < n && chars[i] != ']' {
                    i += 1;
                }
                if i >= n {
                    return Err(()); // Missing ']'
                }
                if start == i {
                    return Err(()); // Empty attribute
                }
                let idx: String = chars[start..i].iter().collect();
                i += 1; // consume ']'
                if idx.chars().all(|c| c.is_ascii_digit()) {
                    specs.push(FieldSpec::IndexNum(idx.parse().map_err(|_| ())?));
                } else {
                    specs.push(FieldSpec::IndexStr(idx));
                }
            }
            _ => return Err(()), // Only '.' or '[' may follow ']'
        }
    }
    Ok((keyname, specs))
}

pub struct MethodFormat {
    /// keyword_arguments: (keyname, specifiers)
    pub fields: Vec<(KeyName, Vec<FieldSpec>)>,
    pub implicit_pos_args: usize,
    pub explicit_pos_args: usize,
}

/// parse_format_method_string (utils.py:637-663)
pub fn parse_format_method_string(s: &str) -> Result<MethodFormat, ()> {
    let mut names = Vec::new();
    collect_string_fields(s, &mut names)?;
    let mut fields = Vec::new();
    let mut implicit = 0usize;
    let mut explicit: std::collections::HashSet<String> = std::collections::HashSet::new();
    for name in names {
        if !name.is_empty() && name.chars().all(|c| c.is_ascii_digit()) {
            explicit.insert(name);
        } else if !name.is_empty() {
            let (keyname, it) = split_format_field_names(&name)?;
            if let KeyName::Num(n) = &keyname {
                explicit.insert(n.to_string());
            }
            fields.push((keyname, it));
        } else {
            implicit += 1;
        }
    }
    Ok(MethodFormat {
        fields,
        implicit_pos_args: implicit,
        explicit_pos_args: explicit.len(),
    })
}

// ---------------------------------------------------------------------------
// the checker
// ---------------------------------------------------------------------------

fn is_classdef(eng: &Engine, g: GNode) -> bool {
    eng.kind_is(g, |k| matches!(k, NodeKind::ClassDef(_)))
}

/// arg_matches_format_type (strings.py:223-239)
fn arg_matches_format_type(eng: &Engine, arg_type: &Value, format_type: char) -> bool {
    if format_type == 's' || format_type == 'r' {
        return true;
    }
    if crate::typecheck::value_is_instance(eng, arg_type) {
        let pt = u::value_pytype(eng, arg_type).unwrap_or_default();
        return match pt.as_str() {
            "builtins.str" => format_type == 'c',
            "builtins.float" => "deEfFgGn%".contains(format_type),
            "builtins.int" => true,
            _ => false,
        };
    }
    true
}

/// python-AST class name of the RHS for E1303 (type(args).__name__)
fn node_class_name(eng: &Engine, g: GNode) -> &'static str {
    let md = eng.md(g.m);
    match &md.tree.nodes[g.n.idx()].kind {
        NodeKind::Const(_) => "Const",
        NodeKind::List { .. } => "List",
        NodeKind::Tuple { .. } => "Tuple",
        NodeKind::Set { .. } => "Set",
        NodeKind::Dict { .. } => "Dict",
        NodeKind::Lambda(_) => "Lambda",
        NodeKind::FunctionDef(_) => "FunctionDef",
        NodeKind::AsyncFunctionDef(_) => "AsyncFunctionDef",
        NodeKind::ListComp(_) => "ListComp",
        NodeKind::SetComp(_) => "SetComp",
        NodeKind::DictComp(_) => "DictComp",
        NodeKind::GeneratorExp(_) => "GeneratorExp",
        _ => "NodeNG",
    }
}

fn is_other_node(k: &NodeKind) -> bool {
    matches!(
        k,
        NodeKind::Const(_)
            | NodeKind::List { .. }
            | NodeKind::Lambda(_)
            | NodeKind::FunctionDef(_)
            | NodeKind::ListComp(_)
            | NodeKind::SetComp(_)
            | NodeKind::GeneratorExp(_)
    )
}

#[derive(Default)]
pub struct StringCk;

impl StringCk {
    /// visit_joinedstr -> _check_interpolation (strings.py:408-417):
    /// W1309 f-string-without-interpolation. Skip when the JoinedStr's parent
    /// is a TemplateStr/FormattedValue (a nested f-string inside a format
    /// spec; TemplateStr is py3.14 t-strings, absent on 3.12), or when any of
    /// its values is a FormattedValue (i.e. it has interpolation). Otherwise
    /// the f-string is constant-only -> emit on the JoinedStr node.
    pub fn visit_joinedstr(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        // parent is FormattedValue -> return (e.g. an f-string used as a
        // format spec of another FormattedValue).
        if let Some(p) = eng.parent(node) {
            if eng.kind_is(p, |k| matches!(k, NodeKind::FormattedValue { .. })) {
                return;
            }
        }
        let values = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::JoinedStr { values } => values.clone(),
                _ => return,
            }
        };
        // any value is a FormattedValue -> has interpolation -> return.
        for v in &values {
            if matches!(
                eng.md(node.m).tree.nodes[v.idx()].kind,
                NodeKind::FormattedValue { .. }
            ) {
                return;
            }
        }
        cx.emit_node(
            "W1309",
            u::lineno(eng, node),
            u::col_offset(eng, node) as i64,
            "Using an f-string that does not have any interpolated variables".to_string(),
        );
    }

    /// visit_binop (strings.py:251-405)
    pub fn visit_binop(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let (left, right) = {
            let md = eng.md(node.m);
            match &md.tree.nodes[node.n.idx()].kind {
                NodeKind::BinOp { left, op, right } if &**op == "%" => {
                    (GNode { m: node.m, n: *left }, GNode { m: node.m, n: *right })
                }
                _ => return,
            }
        };
        let format_string: String = {
            let md = eng.md(left.m);
            match &md.tree.nodes[left.n.idx()].kind {
                NodeKind::Const(ConstValue::Str(s)) => s.to_string(),
                _ => return,
            }
        };
        let line = u::lineno(eng, node);
        let col = u::col_offset(eng, node) as i64;
        let parsed = match parse_format_string(&format_string) {
            Err(PctError::Unsupported(index)) => {
                let formatted: char = format_string.chars().nth(index).unwrap_or('?');
                cx.emit_node(
                    "E1300",
                    line,
                    col,
                    format!(
                        "Unsupported format character {} ({:#02x}) at index {}",
                        u::py_repr_str(&formatted.to_string()),
                        formatted as u32,
                        index
                    ),
                );
                return;
            }
            Err(PctError::Incomplete) => {
                cx.emit_node(
                    "E1301",
                    line,
                    col,
                    "Format string ends in middle of conversion specifier".into(),
                );
                return;
            }
            Ok(p) => p,
        };
        let required_keys = parsed.keys;
        let required_num_args = parsed.num_args;
        let required_key_types = parsed.key_types;
        let required_arg_types = parsed.pos_types;
        if required_keys.is_empty() && required_num_args == 0 {
            cx.emit_node(
                "W1310",
                line,
                col,
                "Using formatting for a string that does not have any interpolated variables"
                    .into(),
            );
            return;
        }
        if !required_keys.is_empty() && required_num_args > 0 {
            cx.emit_node(
                "E1302",
                line,
                col,
                "Mixing named and unnamed conversion specifiers in format string".into(),
            );
        } else if !required_keys.is_empty() {
            // named specifiers
            let dict_items: Option<Vec<(GNode, GNode)>> = {
                let md = eng.md(right.m);
                match &md.tree.nodes[right.n.idx()].kind {
                    NodeKind::Dict { items } => Some(
                        items
                            .iter()
                            .map(|&(k, v)| (GNode { m: right.m, n: k }, GNode { m: right.m, n: v }))
                            .collect(),
                    ),
                    _ => None,
                }
            };
            if let Some(items) = dict_items {
                let mut keys: indexmap::IndexSet<String> = indexmap::IndexSet::new();
                let mut unknown_keys = false;
                for (k, _) in &items {
                    let md = eng.md(k.m);
                    match &md.tree.nodes[k.n.idx()].kind {
                        NodeKind::Const(ConstValue::Str(s)) => {
                            keys.insert(s.to_string());
                        }
                        NodeKind::Const(c) => {
                            let kv = const_repr_for_msg(c);
                            drop(md);
                            cx.emit_node(
                                "W1300",
                                line,
                                col,
                                format!("Format string dictionary key should be a string, not {kv}"),
                            );
                        }
                        _ => unknown_keys = true,
                    }
                }
                if !unknown_keys {
                    for key in &required_keys {
                        if !keys.contains(key) {
                            cx.emit_node(
                                "E1304",
                                line,
                                col,
                                u::format_template(
                                    "Missing key %r in format string dictionary",
                                    &[key],
                                ),
                            );
                        }
                    }
                }
                for key in &keys {
                    if !required_keys.contains(key) {
                        cx.emit_node(
                            "W1301",
                            line,
                            col,
                            u::format_template("Unused key %r in format string dictionary", &[key]),
                        );
                    }
                }
                // E1307 per dict entry
                enum KeyLookup {
                    Str(String),
                    NonStr,
                }
                for (k, arg) in &items {
                    let kstr: Option<KeyLookup> = {
                        let md = eng.md(k.m);
                        match &md.tree.nodes[k.n.idx()].kind {
                            NodeKind::Const(ConstValue::Str(s)) => {
                                Some(KeyLookup::Str(s.to_string()))
                            }
                            NodeKind::Const(_) => Some(KeyLookup::NonStr),
                            _ => None,
                        }
                    };
                    let Some(kstr) = kstr else { continue };
                    let format_type = match &kstr {
                        KeyLookup::Str(s) => required_key_types.get(s).copied(),
                        KeyLookup::NonStr => None,
                    };
                    let arg_type = u::safe_infer(eng, cx.caches, *arg);
                    if let (Some(ft), Some(at)) = (format_type, arg_type) {
                        if !at.is_uninferable() && !arg_matches_format_type(eng, &at, ft) {
                            let pt = u::value_pytype(eng, &at).unwrap_or_default();
                            cx.emit_node(
                                "E1307",
                                line,
                                col,
                                u::format_template(
                                    "Argument %r does not match format type %r",
                                    &[&pt, &ft.to_string()],
                                ),
                            );
                        }
                    }
                }
            } else {
                let is_other_or_tuple = {
                    let md = eng.md(right.m);
                    let k = &md.tree.nodes[right.n.idx()].kind;
                    is_other_node(k) || matches!(k, NodeKind::Tuple { .. })
                };
                if is_other_or_tuple {
                    cx.emit_node(
                        "E1303",
                        line,
                        col,
                        u::format_template(
                            "Expected mapping for format string, not %s",
                            &[node_class_name(eng, right)],
                        ),
                    );
                }
                // else: Name/expression RHS -> silent
            }
        } else {
            // unnamed specifiers
            let mut args_elts: Vec<ArgElt> = Vec::new();
            let mut num_args: Option<usize> = None;
            let md = eng.md(right.m);
            let kind_tag = &md.tree.nodes[right.n.idx()].kind;
            if matches!(kind_tag, NodeKind::Tuple { .. }) {
                drop(md);
                let rhs_tuple = u::safe_infer(eng, cx.caches, right);
                if let Some(v) = rhs_tuple {
                    if let Some(elts) = container_elts(eng, &v) {
                        num_args = Some(elts.len());
                        args_elts = elts;
                    }
                }
            } else if is_other_node(kind_tag)
                || matches!(kind_tag, NodeKind::Dict { .. } | NodeKind::DictComp(_))
            {
                drop(md);
                args_elts = vec![ArgElt::Node(right)];
                num_args = Some(1);
            } else if matches!(kind_tag, NodeKind::Name { .. }) {
                drop(md);
                let inferred = u::safe_infer(eng, cx.caches, right);
                match inferred {
                    Some(Value::Node(g))
                        if eng.kind_is(g, |k| matches!(k, NodeKind::Tuple { .. })) =>
                    {
                        let md = eng.md(g.m);
                        if let NodeKind::Tuple { elts, .. } = &md.tree.nodes[g.n.idx()].kind {
                            num_args = Some(elts.len());
                            args_elts = elts
                                .iter()
                                .map(|&e| ArgElt::Node(GNode { m: g.m, n: e }))
                                .collect();
                        }
                    }
                    Some(v @ Value::SynthSeq { kind: pyinfer::value::SeqKind::Tuple, .. }) => {
                        if let Some(elts) = container_elts(eng, &v) {
                            num_args = Some(elts.len());
                            args_elts = elts;
                        }
                    }
                    Some(v) if is_const_value(eng, &v) => {
                        args_elts = vec![ArgElt::Val(v)];
                        num_args = Some(1);
                    }
                    _ => {}
                }
            }
            if let Some(num_args) = num_args {
                if num_args > required_num_args {
                    cx.emit_node(
                        "E1305",
                        line,
                        col,
                        "Too many arguments for format string".into(),
                    );
                } else if num_args < required_num_args {
                    cx.emit_node(
                        "E1306",
                        line,
                        col,
                        "Not enough arguments for format string".into(),
                    );
                }
                for (arg, &format_type) in args_elts.iter().zip(required_arg_types.iter()) {
                    let arg_type: Option<Value> = match arg {
                        ArgElt::Node(g) => u::safe_infer(eng, cx.caches, *g),
                        ArgElt::Val(v) => Some(v.clone()),
                    };
                    if let Some(at) = arg_type {
                        if !at.is_uninferable() && !arg_matches_format_type(eng, &at, format_type) {
                            let pt = u::value_pytype(eng, &at).unwrap_or_default();
                            cx.emit_node(
                                "E1307",
                                line,
                                col,
                                u::format_template(
                                    "Argument %r does not match format type %r",
                                    &[&pt, &format_type.to_string()],
                                ),
                            );
                        }
                    }
                }
            }
        }
    }

    /// visit_call (strings.py:419-437): bound-method str checks
    pub fn visit_call(&mut self, cx: &mut WalkCx, node: GNode) {
        let eng = cx.eng;
        let Some((fnode, args, _)) = crate::typecheck::call_parts(eng, node) else { return };
        let func = u::safe_infer(eng, cx.caches, fnode);
        let Some(Value::BoundMethod { func: meth, bound }) = func else { return };
        let bound_name = match crate::typecheck::value_name(eng, &bound) {
            Some(n) if n == "str" || n == "unicode" || n == "bytes" => n,
            _ => return,
        };
        let meth_name = eng.node_name(meth).unwrap_or_default();
        if matches!(meth_name.as_str(), "strip" | "lstrip" | "rstrip") && !args.is_empty() {
            let arg = u::safe_infer(eng, cx.caches, args[0]);
            let s: String = match arg {
                Some(Value::Node(g)) => {
                    let md = eng.md(g.m);
                    match &md.tree.nodes[g.n.idx()].kind {
                        NodeKind::Const(ConstValue::Str(s)) => s.to_string(),
                        _ => return,
                    }
                }
                Some(Value::SynthConst(c)) => match &*c {
                    ConstValue::Str(s) => s.to_string(),
                    _ => return,
                },
                _ => return,
            };
            let set: std::collections::HashSet<char> = s.chars().collect();
            if s.chars().count() != set.len() {
                cx.emit_node(
                    "E1310",
                    u::lineno(eng, node),
                    u::col_offset(eng, node) as i64,
                    u::format_template("Suspicious argument in %s.%s call", &[&bound_name, &meth_name]),
                );
            }
        } else if meth_name == "format" {
            self.check_new_format(cx, node, &bound);
        }
    }

    /// _check_new_format (strings.py:452-534)
    fn check_new_format(&mut self, cx: &mut WalkCx, node: GNode, bound: &Value) {
        let eng = cx.eng;
        let (fnode, args, keywords) = match crate::typecheck::call_parts(eng, node) {
            Some(p) => p,
            None => return,
        };
        // node.func is Attribute and node.func.expr not Const -> return
        {
            let md = eng.md(fnode.m);
            if let NodeKind::Attribute { expr, .. } = &md.tree.nodes[fnode.n.idx()].kind {
                if !matches!(md.tree.nodes[expr.idx()].kind, NodeKind::Const(_)) {
                    return;
                }
            }
        }
        // starargs / kwargs -> bail
        let has_star = args.iter().any(|&a| {
            eng.kind_is(a, |k| matches!(k, NodeKind::Starred { .. }))
        });
        let has_kwargs = keywords.iter().any(|&k| {
            let md = eng.md(k.m);
            matches!(&md.tree.nodes[k.n.idx()].kind, NodeKind::Keyword { arg: None, .. })
        });
        if has_star || has_kwargs {
            return;
        }
        // strnode = next(func.bound.infer())
        let strval: Option<String> = match bound {
            Value::Node(g) => {
                let mut first: Option<Value> = None;
                let _ = eng.infer_to(*g, &Ctx::new(), &mut |v| {
                    first = Some(v);
                    pyinfer::value::Drive::Stop
                });
                match first {
                    Some(Value::Node(g2)) => {
                        let md = eng.md(g2.m);
                        match &md.tree.nodes[g2.n.idx()].kind {
                            NodeKind::Const(ConstValue::Str(s)) => Some(s.to_string()),
                            _ => None,
                        }
                    }
                    Some(Value::SynthConst(c)) => match &*c {
                        ConstValue::Str(s) => Some(s.to_string()),
                        _ => None,
                    },
                    _ => None,
                }
            }
            Value::SynthConst(c) => match &**c {
                ConstValue::Str(s) => Some(s.to_string()),
                _ => None,
            },
            _ => None,
        };
        let Some(strval) = strval else { return };
        // CallSite.from_call(node)
        let cc = CallCtx {
            id: eng.next_callctx_id(),
            args: RefCell::new(args.iter().map(|&a| NV::N(a)).collect()),
            keywords: RefCell::new(
                keywords
                    .iter()
                    .map(|&k| {
                        let md = eng.md(k.m);
                        match &md.tree.nodes[k.n.idx()].kind {
                            NodeKind::Keyword { arg, value } => {
                                let nm = arg.map(|s| eng.g(&md, s));
                                (nm, GNode { m: k.m, n: *value })
                            }
                            _ => (None, k),
                        }
                    })
                    .collect(),
            ),
            callee: RefCell::new(None),
        };
        let site = eng.call_site_from(&cc, &Ctx::new());
        let line = u::lineno(eng, node);
        let col = u::col_offset(eng, node) as i64;
        let parsed = match parse_format_method_string(&strval) {
            Ok(p) => p,
            Err(()) => {
                cx.emit_node(
                    "W1302",
                    line,
                    col,
                    u::format_template("Invalid format string", &[]),
                );
                return;
            }
        };
        let fields = parsed.fields;
        let mut num_args = parsed.implicit_pos_args;
        let manual_pos = parsed.explicit_pos_args;
        let positional_arguments = site.positional_arguments();
        let named_arguments: Vec<GSym> =
            site.keyword_arguments().iter().map(|(k, _)| *k).collect();
        let named_fields: indexmap::IndexSet<String> = fields
            .iter()
            .filter_map(|(k, _)| match k {
                KeyName::Str(s) => Some(s.clone()),
                KeyName::Num(_) => None,
            })
            .collect();
        if num_args > 0 && manual_pos > 0 {
            cx.emit_node(
                "W1305",
                line,
                col,
                "Format string contains both automatic field numbering and manual field specification"
                    .into(),
            );
            return;
        }
        let mut check_args = false;
        num_args += named_fields.iter().filter(|f| f.is_empty()).count();
        if !named_fields.is_empty() {
            for field in &named_fields {
                if !field.is_empty()
                    && !named_arguments.iter().any(|&k| eng.sname(k) == *field)
                {
                    cx.emit_node(
                        "W1303",
                        line,
                        col,
                        u::format_template("Missing keyword argument %r for format string", &[field]),
                    );
                }
            }
            for field in &named_arguments {
                let fs = eng.sname(*field);
                if !named_fields.contains(&fs) {
                    cx.emit_node(
                        "W1304",
                        line,
                        col,
                        u::format_template("Unused format argument %r", &[&fs]),
                    );
                }
            }
            if num_args == 0 {
                num_args = manual_pos;
            }
            if !positional_arguments.is_empty() || num_args > 0 {
                let empty = named_fields.iter().any(|f| f.is_empty());
                if !named_arguments.is_empty() || empty {
                    check_args = true;
                }
            }
        } else {
            check_args = true;
        }
        if check_args {
            if num_args == 0 {
                num_args = manual_pos;
            }
            if num_args == 0 {
                cx.emit_node(
                    "W1310",
                    line,
                    col,
                    "Using formatting for a string that does not have any interpolated variables"
                        .into(),
                );
                return;
            }
            if positional_arguments.len() > num_args {
                cx.emit_node("E1305", line, col, "Too many arguments for format string".into());
            } else if positional_arguments.len() < num_args {
                cx.emit_node("E1306", line, col, "Not enough arguments for format string".into());
            }
        }
        self.detect_vacuous_formatting(cx, node, &positional_arguments);
        self.check_new_format_specifiers(cx, node, &fields, &site);
    }

    /// _detect_vacuous_formatting — W1308
    fn detect_vacuous_formatting(&mut self, cx: &mut WalkCx, node: GNode, positional: &[NV]) {
        let eng = cx.eng;
        let mut counter: indexmap::IndexMap<String, usize> = indexmap::IndexMap::new();
        for arg in positional {
            if let NV::N(g) = arg {
                let md = eng.md(g.m);
                if let NodeKind::Name { name } = &md.tree.nodes[g.n.idx()].kind {
                    let nm = md.tree.s(*name).to_string();
                    *counter.entry(nm).or_insert(0) += 1;
                }
            }
        }
        for (name, count) in counter {
            if count == 1 {
                continue;
            }
            cx.emit_node(
                "W1308",
                u::lineno(eng, node),
                u::col_offset(eng, node) as i64,
                u::format_template(
                    "Duplicate string formatting argument %r, consider passing as named argument",
                    &[&name],
                ),
            );
        }
    }

    /// _check_new_format_specifiers (strings.py:537-636) — W1306/W1307 +
    /// the getattr/getitem inference burn.
    fn check_new_format_specifiers(
        &mut self,
        cx: &mut WalkCx,
        node: GNode,
        fields: &[(KeyName, Vec<FieldSpec>)],
        site: &pyinfer::calls::CallSite,
    ) {
        let eng = cx.eng;
        for (key, specifiers) in fields {
            let key = match key {
                KeyName::Str(s) if s.is_empty() => KeyName::Num(0),
                other => other.clone(),
            };
            let argname: Option<NV> = match &key {
                KeyName::Num(n) => {
                    // get_argument_from_call(node, key)
                    let Some((_, args, _)) = crate::typecheck::call_parts(eng, node) else {
                        continue;
                    };
                    match args.get(*n as usize) {
                        Some(&a) => Some(NV::N(a)),
                        None => continue, // NoSuchArgumentError
                    }
                }
                KeyName::Str(s) => {
                    let mut found = None;
                    for (k, v) in site.keyword_arguments() {
                        if eng.sname(k) == *s {
                            found = Some(v);
                            break;
                        }
                    }
                    match found {
                        Some(v) => Some(v),
                        None => continue,
                    }
                }
            };
            let Some(argname) = argname else { continue };
            if matches!(&argname, NV::V(Value::Uninferable)) {
                continue;
            }
            let argument: Option<Value> = match &argname {
                NV::N(g) => u::safe_infer(eng, cx.caches, *g),
                NV::V(v) => Some(v.clone()),
            };
            if specifiers.is_empty() {
                continue;
            }
            let Some(mut previous) = argument else { continue };
            // argument.parent is Arguments -> skip (strings.py:574-577). astroid
            // parents the synthetic *args Tuple / **kwargs Dict to the
            // Arguments node, so a value inferred FROM a vararg/kwarg parameter
            // is ignored. Our synthetic containers carry no parent, so we
            // detect the case from the source: a bare Name argument that
            // resolves to a vararg/kwarg AssignName.
            if let Value::Node(g) = &previous {
                if let Some(p) = eng.parent(*g) {
                    if eng.kind_is(p, |k| matches!(k, NodeKind::Arguments(_))) {
                        continue;
                    }
                }
            }
            if let NV::N(g) = &argname {
                if argname_is_vararg_or_kwarg(eng, *g) {
                    continue;
                }
            }
            let mut parsed: Vec<(bool, String)> = Vec::new();
            'specloop: for spec in specifiers {
                if previous.is_uninferable() {
                    break;
                }
                match spec {
                    FieldSpec::Attr(attr) => {
                        parsed.push((true, attr.clone()));
                        let sym = eng.sym(attr);
                        match crate::typecheck::value_getattr(eng, &previous, sym) {
                            Ok(vals) => {
                                // previous = previous.getattr(specifier)[0]
                                match vals.first() {
                                    Some(NV::N(g)) => previous = Value::Node(*g),
                                    Some(NV::V(v)) => previous = v.clone(),
                                    None => break 'specloop,
                                }
                            }
                            Err(()) => {
                                // has_dynamic_getattr check
                                let dynamic = match &previous {
                                    Value::Inst { cls, .. } | Value::ExcInst { cls, .. } => {
                                        eng.has_dynamic_getattr(*cls, &Ctx::new())
                                    }
                                    _ => false,
                                };
                                if !dynamic {
                                    let path = get_access_path(&key, &parsed);
                                    cx.emit_node(
                                        "W1306",
                                        u::lineno(eng, node),
                                        u::col_offset(eng, node) as i64,
                                        u::format_template(
                                            "Missing format attribute %r in format specifier %r",
                                            &[attr, &path],
                                        ),
                                    );
                                }
                                break 'specloop;
                            }
                        }
                    }
                    FieldSpec::IndexNum(_) | FieldSpec::IndexStr(_) => {
                        // `specifier` is the int/str from formatter_field_name_split
                        // (utils.parse_format_method_string): numeric field parts
                        // are Python ints, named parts are str. The W1307 message
                        // formats `%r % specifier` — int repr has NO quotes.
                        let (spec_str, spec_repr, idx_val): (String, String, Value) = match spec {
                            FieldSpec::IndexNum(n) => (
                                n.to_string(),
                                n.to_string(),
                                Value::SynthConst(std::rc::Rc::new(ConstValue::Int(pyast::tree::IntValue::Small(*n)))),
                            ),
                            FieldSpec::IndexStr(s) => (
                                s.clone(),
                                u::py_repr_str(s),
                                Value::SynthConst(std::rc::Rc::new(ConstValue::Str(
                                    s.clone().into_boxed_str(),
                                ))),
                            ),
                            _ => unreachable!(),
                        };
                        parsed.push((false, spec_str.clone()));
                        let mut warn_error = false;
                        // hasattr(previous, "getitem") — Python hasattr on the
                        // astroid NODE: True for List/Tuple/Dict/Set/Const/
                        // ClassDef/Instance/FrozenSet, False for FunctionDef/
                        // Module/BoundMethod/Generator/Uninferable. An Instance
                        // (e.g. `list(extra.keys())` -> generic list Instance)
                        // HAS getitem, and Instance.getitem(Const(0)) raises
                        // AstroidTypeError -> W1307 (salt args.py:465).
                        let has_getitem_attr = match &previous {
                            Value::Node(g) => {
                                let md = eng.md(g.m);
                                matches!(
                                    &md.tree.nodes[g.n.idx()].kind,
                                    NodeKind::Const(_)
                                        | NodeKind::List { .. }
                                        | NodeKind::Tuple { .. }
                                        | NodeKind::Dict { .. }
                                        | NodeKind::Set { .. }
                                        | NodeKind::ClassDef(_)
                                ) || is_classdef(eng, *g)
                            }
                            Value::Inst { .. }
                            | Value::ExcInst { .. }
                            | Value::SynthConst(_)
                            | Value::SynthSeq { .. }
                            | Value::SynthDict { .. }
                            | Value::FrozenSet { .. } => true,
                            _ => false,
                        };
                        if has_getitem_attr {
                            match eng.getitem(&previous, &idx_val, &Ctx::new()) {
                                Ok(nv) => {
                                    previous = match nv {
                                        NV::N(g) => Value::Node(g),
                                        NV::V(v) => v,
                                    };
                                    if previous.is_uninferable() {
                                        break 'specloop;
                                    }
                                }
                                Err(ErrKind::AstroidIndex)
                                | Err(ErrKind::AstroidType)
                                | Err(ErrKind::Attribute) => warn_error = true,
                                Err(_) => break 'specloop,
                            }
                        } else {
                            // previous.getattr("__getitem__") probe
                            let sym = eng.sym("__getitem__");
                            match crate::typecheck::value_getattr(eng, &previous, sym) {
                                Ok(_) => break 'specloop,
                                Err(()) => warn_error = true,
                            }
                        }
                        if warn_error {
                            let path = get_access_path(&key, &parsed);
                            cx.emit_node(
                                "W1307",
                                u::lineno(eng, node),
                                u::col_offset(eng, node) as i64,
                                u::format_template(
                                    "Using invalid lookup key %s in format specifier %r",
                                    &[&spec_repr, &path],
                                ),
                            );
                            break 'specloop;
                        }
                    }
                }
                // previous = next(previous.infer())
                if let Value::Node(g) = previous.clone() {
                    let mut first: Option<Value> = None;
                    let end = eng.infer_to(g, &Ctx::new(), &mut |v| {
                        first = Some(v);
                        pyinfer::value::Drive::Stop
                    });
                    match first {
                        Some(v) => previous = v,
                        None => {
                            let _ = end;
                            break 'specloop;
                        }
                    }
                }
            }
        }
    }
}

enum ArgElt {
    Node(GNode),
    Val(Value),
}

fn container_elts(eng: &Engine, v: &Value) -> Option<Vec<ArgElt>> {
    match v {
        Value::Node(g) => {
            let md = eng.md(g.m);
            match &md.tree.nodes[g.n.idx()].kind {
                NodeKind::Tuple { elts, .. }
                | NodeKind::List { elts, .. }
                | NodeKind::Set { elts, .. } => Some(
                    elts.iter()
                        .map(|&e| ArgElt::Node(GNode { m: g.m, n: e }))
                        .collect(),
                ),
                _ => None,
            }
        }
        Value::SynthSeq { elems, .. } => {
            Some(elems.iter().map(|e| ArgElt::Val(e.clone())).collect())
        }
        Value::FrozenSet { elems } => {
            Some(elems.iter().map(|e| ArgElt::Val(e.clone())).collect())
        }
        _ => None,
    }
}

fn is_const_value(eng: &Engine, v: &Value) -> bool {
    match v {
        Value::SynthConst(_) => true,
        Value::Node(g) => eng.kind_is(*g, |k| matches!(k, NodeKind::Const(_))),
        _ => false,
    }
}

/// get_access_path (strings.py:210-221)
fn get_access_path(key: &KeyName, parts: &[(bool, String)]) -> String {
    let mut path = String::new();
    for (is_attr, spec) in parts {
        if *is_attr {
            path.push('.');
            path.push_str(spec);
        } else {
            // f"[{specifier!r}]" — int specs repr without quotes
            if spec.chars().all(|c| c.is_ascii_digit()) && !spec.is_empty() {
                path.push_str(&format!("[{spec}]"));
            } else {
                path.push_str(&format!("[{}]", u::py_repr_str(spec)));
            }
        }
    }
    let k = match key {
        KeyName::Num(n) => n.to_string(),
        KeyName::Str(s) => s.clone(),
    };
    format!("{k}{path}")
}

/// python str(value) of a Const for the W1300 args
fn const_repr_for_msg(c: &ConstValue) -> String {
    match c {
        ConstValue::None => "None".into(),
        ConstValue::Bool(b) => if *b { "True".into() } else { "False".into() },
        ConstValue::Int(pyast::tree::IntValue::Small(i)) => i.to_string(),
        ConstValue::Int(pyast::tree::IntValue::Big(s)) => s.to_string(),
        ConstValue::Float(f) => pyast::pyrepr::repr_float(*f),
        ConstValue::Complex { .. } => "complex".into(),
        ConstValue::Str(s) => s.to_string(),
        ConstValue::Bytes(b) => {
            let inner: String = b.iter().map(|&c| {
                if (0x20..0x7f).contains(&c) && c != b'\'' && c != b'\\' {
                    (c as char).to_string()
                } else {
                    format!("\\x{c:02x}")
                }
            }).collect();
            format!("b'{inner}'")
        }
        ConstValue::Ellipsis => "Ellipsis".into(),
        ConstValue::NotImplemented => "NotImplemented".into(),
        ConstValue::StrSurrogate(_) => String::new(),
    }
}

/// Whether the call-argument node `g` is a bare Name that resolves to a
/// `*args`/`**kwargs` parameter. astroid infers such a Name to a synthetic
/// Tuple/Dict parented to the Arguments node; pylint's W1306/W1307 then skip it
/// (strings.py:574-577 "Ignore any object coming from an argument"). Our
/// synthetic containers carry no parent, so we replicate the test on the
/// source: resolve the Name and check whether any binding is the vararg/kwarg
/// AssignName of an Arguments node. Uses the engine lookup whose (node, name)
/// key was already populated by the preceding safe_infer (cache hit, no burn).
fn argname_is_vararg_or_kwarg(eng: &Engine, g: GNode) -> bool {
    let name_sym = {
        let md = eng.md(g.m);
        match &md.tree.nodes[g.n.idx()].kind {
            NodeKind::Name { name } => eng.g(&md, *name),
            _ => return false,
        }
    };
    let res = eng.lookup(g, name_sym);
    for nv in res.1.iter() {
        let bind = match nv {
            NV::N(b) => *b,
            _ => continue,
        };
        let md = eng.md(bind.m);
        match &md.tree.nodes[bind.n.idx()].kind {
            // Our lookup resolves a *args/**kwargs name to the Arguments
            // frame directly: check its vararg/kwarg sym (local Sym -> GSym).
            NodeKind::Arguments(a) => {
                let vararg_g = a.vararg.map(|s| eng.g(&md, s));
                let kwarg_g = a.kwarg.map(|s| eng.g(&md, s));
                if vararg_g == Some(name_sym) || kwarg_g == Some(name_sym) {
                    return true;
                }
            }
            // ...or to the AssignName itself, whose Arguments parent's
            // vararg/kwarg node matches.
            NodeKind::AssignName { .. } => {
                let bn = bind.n;
                drop(md);
                if let Some(parent) = eng.parent(bind) {
                    let pmd = eng.md(parent.m);
                    if let NodeKind::Arguments(a) = &pmd.tree.nodes[parent.n.idx()].kind {
                        if a.vararg_node == Some(bn) || a.kwarg_node == Some(bn) {
                            return true;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    false
}
