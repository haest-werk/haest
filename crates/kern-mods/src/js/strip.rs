//! TypeScript to JavaScript for a `.ts` mod: types stripped, nothing else
//! changed (no downlevelling, ES modules kept). Pure Rust: no node, no bun.

use std::path::Path;

use oxc::{
    allocator::Allocator,
    codegen::Codegen,
    diagnostics::OxcDiagnostic,
    parser::Parser,
    semantic::SemanticBuilder,
    span::SourceType,
    transformer::{TransformOptions, Transformer},
};

/// Diagnostics, each with the line it points at.
fn describe(errors: &[OxcDiagnostic], source: &str) -> String {
    errors
        .iter()
        .take(3)
        .map(|e| {
            let offset = e
                .labels
                .as_ref()
                .and_then(|l| l.first())
                .map(|l| l.offset());
            match offset {
                Some(at) => {
                    let line = source[..at.min(source.len())].matches('\n').count() + 1;
                    format!("line {line}: {e}")
                }
                None => e.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

pub(crate) fn strip_types(path: &Path, source: &str) -> Result<String, String> {
    let alloc = Allocator::default();
    let parsed = Parser::new(&alloc, source, SourceType::ts()).parse();
    if parsed.panicked || !parsed.errors.is_empty() {
        return Err(format!(
            "it is not valid TypeScript: {}",
            describe(&parsed.errors, source)
        ));
    }
    let mut program = parsed.program;
    let scoping = SemanticBuilder::new()
        .build(&program)
        .semantic
        .into_scoping();
    let done = Transformer::new(&alloc, path, &TransformOptions::default())
        .build_with_scoping(scoping, &mut program);
    if !done.errors.is_empty() {
        return Err(format!(
            "its TypeScript could not be stripped: {}",
            describe(&done.errors, source)
        ));
    }
    Ok(Codegen::new().build(&program).code)
}
