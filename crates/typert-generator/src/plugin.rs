//! Build-plugin behaviors: decorator lowering of TypeScript dependencies.

use crate::Result;

/// Lowered module text and its source map.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranspileOutput {
    /// Lowered JavaScript without a trailing source-map comment.
    pub code: String,
    /// JSON source map text.
    pub map: Option<String>,
}

/// Lowers standard decorators in a TypeScript dependency the way the source
/// build plugin does; modules without decorator syntax are left untouched.
///
/// # Errors
/// Propagates compiler loading and transpilation failures.
#[cfg(not(target_arch = "wasm32"))]
pub fn transpile(code: &str, id: &str) -> Result<Option<TranspileOutput>> {
    use crate::analyzer::native::{js, locate_library};

    let file = id.split('?').next().unwrap_or(id).to_owned();
    if !is_typescript_file(&file) || !has_decorator_syntax(code) {
        return Ok(None);
    }
    let library = locate_library(
        std::path::Path::new(&file)
            .parent()
            .unwrap_or(std::path::Path::new(".")),
    )?;
    let mut compiler = crate::analyzer::Compiler::load(&library)?;
    compiler.run(|scope, ts| {
        let constants = compiler_constants(scope, ts)?;
        let mut options = serde_json::json!({
            "target": constants.0,
            "module": constants.1,
            "sourceMap": true,
        });
        if file.ends_with('x') {
            options["jsx"] = serde_json::json!(constants.2);
        }
        let request = serde_json::json!({ "fileName": file, "compilerOptions": options });
        let request = js::from_json(scope, &request)?;
        let code = js::string(scope, code);
        let result = js::call(scope, ts, "transpileModule", &[code, request])?;
        let output = js::get_string(scope, result, "outputText")?;
        let map = js::get_optional_string(scope, result, "sourceMapText")?;
        Ok(Some(TranspileOutput {
            code: strip_source_map_comment(&output),
            map,
        }))
    })
}

#[cfg(not(target_arch = "wasm32"))]
fn compiler_constants<'s>(
    scope: &mut crate::analyzer::native::js::Scope<'s, '_>,
    ts: crate::analyzer::native::js::Val<'s>,
) -> Result<(u32, u32, u32)> {
    use crate::analyzer::native::js;
    let target = js::get_path(scope, ts, "ScriptTarget.ES2024")?;
    let module = js::get_path(scope, ts, "ModuleKind.ESNext")?;
    let jsx = js::get_path(scope, ts, "JsxEmit.ReactJSX")?;
    Ok((
        js::integer(target.number_value(scope).unwrap_or_default()),
        js::integer(module.number_value(scope).unwrap_or_default()),
        js::integer(jsx.number_value(scope).unwrap_or_default()),
    ))
}

/// `/\.[cm]?tsx?$/`
#[must_use]
#[expect(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "the source build plugin matches extensions case-sensitively"
)]
pub fn is_typescript_file(file: &str) -> bool {
    let stem = file.strip_suffix('x').unwrap_or(file);
    stem.ends_with(".ts") || stem.ends_with(".cts") || stem.ends_with(".mts")
}

/// `/^\s*@[A-Za-z_$][\w$]*/m`
#[must_use]
pub fn has_decorator_syntax(code: &str) -> bool {
    code.split(['\n', '\r', '\u{2028}', '\u{2029}'])
        .any(|line| {
            let trimmed = line.trim_start_matches(crate::analyzer::native::jstext::is_js_space);
            let mut characters = trimmed.chars();
            characters.next() == Some('@')
                && characters.next().is_some_and(|first| {
                    first.is_ascii_alphabetic() || first == '_' || first == '$'
                })
        })
}

/// `output.replace(/\n?\/\/# sourceMappingURL=.*$/u, '\n')`
#[must_use]
pub fn strip_source_map_comment(output: &str) -> String {
    const MARKER: &str = "//# sourceMappingURL=";
    let Some(index) = output.rfind(MARKER) else {
        return output.to_owned();
    };
    let tail = &output[index + MARKER.len()..];
    if tail.contains(['\n', '\r', '\u{2028}', '\u{2029}']) {
        return output.to_owned();
    }
    let head = &output[..index];
    let head = head.strip_suffix('\n').unwrap_or(head);
    format!("{head}\n")
}
