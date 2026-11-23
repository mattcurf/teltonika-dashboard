//! Compiles the browser code (web/src/app.ts) to JavaScript in OUT_DIR, which the server
//! embeds. oxc strips the types without checking them; `tsc -p web --noEmit` checks them.

use std::path::Path;

use oxc::allocator::Allocator;
use oxc::codegen::Codegen;
use oxc::parser::Parser;
use oxc::semantic::SemanticBuilder;
use oxc::span::SourceType;
use oxc::transformer::{TransformOptions, Transformer};

fn main() {
    let web = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web");
    println!("cargo:rerun-if-changed={}", web.display());
    let entry = web.join("src/app.ts");
    let source = std::fs::read_to_string(&entry).expect("read web/src/app.ts");

    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, &source, SourceType::ts()).parse();
    if !parsed.errors.is_empty() || parsed.panicked {
        for e in &parsed.errors {
            println!("cargo:warning={e}");
        }
        panic!("web/src/app.ts has syntax errors; see the warnings above");
    }
    let mut program = parsed.program;
    let scoping = SemanticBuilder::new()
        .build(&program)
        .semantic
        .into_scoping();
    let result = Transformer::new(&allocator, &entry, &TransformOptions::default())
        .build_with_scoping(scoping, &mut program);
    if !result.errors.is_empty() {
        for e in &result.errors {
            println!("cargo:warning={e}");
        }
        panic!("could not strip the types from web/src/app.ts");
    }
    let js = Codegen::new().build(&program).code;
    let out = Path::new(&std::env::var("OUT_DIR").unwrap()).join("app.js");
    std::fs::write(out, js).expect("write app.js");
}
