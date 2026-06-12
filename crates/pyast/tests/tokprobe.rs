use ruff_python_parser::{Mode, ParseOptions};
use ruff_text_size::Ranged;

fn dump(src: &str) {
    println!("=== {:?}", src);
    let options = ParseOptions::from(Mode::Module)
        .with_target_version(ruff_python_ast::PythonVersion::PY312);
    let parsed = ruff_python_parser::parse_unchecked(src, options);
    for t in parsed.tokens() {
        println!(
            "  {:?} {}..{} {:?}",
            t.kind(),
            t.range().start().to_u32(),
            t.range().end().to_u32(),
            &src[t.range()]
        );
    }
}

#[test]
fn probe() {
    dump("x = 1\n");
    dump("x = 1");
    dump("# only comment");
    dump("if x:\n    pass\ny = 2\n");
    dump("def f():\n\n    pass\n");
    dump("x = 1\r\ny = 2\r\n");
    dump("s = \"\"\"a\nb\nc\"\"\"\nz=1\n");
    dump("f'{x} y'\n");
    dump("x = 1 ;\n");
    dump("x = (1,\n  2)\n");
    dump("\"\"\"doc\"\"\"\rx = 1\r");
    dump("x = 1\n\n\n");
    dump("");
    dump("   \n");
    dump("if x:\n    pass");
}

#[test]
fn probe2() {
    dump("x = 1\n   ");
    dump("x = 1\n# end");
    dump("f'''{x}\n {y} t\n'''\n");
    dump("\x0cx = 1\n");
    dump("def f():\n\x0c    pass\n");
    dump("x = 1  \n");
    dump("# c\n");
    dump("x = 1\n# end\n");
    dump("@dec\ndef f(): pass\n");
    dump("x = 1\n\x0c");
}

#[test]
fn probe3() {
    dump("class A:\n\\\n    pass\n");
    dump("class B:\n  \\\n    \"\"\"Some\n    \\\n    Doc\n    \"\"\"\n");
}
