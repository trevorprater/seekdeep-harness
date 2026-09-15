//! Source Map v3 artifacts built from the post-bindgen WASM module's DWARF line tables.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::Context as _;
use base64::Engine as _;
use gimli::{Dwarf, EndianSlice, LittleEndian, SectionId};
use path_clean::PathClean as _;
use serde::Serialize;
use wasmparser::{Parser, Payload};

/// Source Map v3 document, including source bytes for offline browser debugging.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceMap {
    /// Fixed Source Map protocol version.
    pub version: u8,
    /// Artifact this map describes.
    pub file: String,
    /// Original source URLs in mapping index order.
    pub sources: Vec<String>,
    /// Original source text when present on the build machine.
    pub sources_content: Vec<Option<String>>,
    /// Symbol names; line-only mappings do not require name indices.
    pub names: Vec<String>,
    /// Base64 VLQ position mappings.
    pub mappings: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Location {
    offset: u64,
    file: PathBuf,
    line: u64,
    column: u64,
}

fn dwarf_locations(wasm: &[u8]) -> anyhow::Result<Vec<Location>> {
    let mut sections = BTreeMap::new();
    let mut code_start = None;
    let mut code_end = 0;
    for payload in Parser::new(0).parse_all(wasm) {
        match payload? {
            Payload::CustomSection(section) => {
                sections.insert(section.name(), section.data());
            }
            Payload::CodeSectionStart { range, .. } => {
                code_start = Some(u64::try_from(range.start)?);
                code_end = u64::try_from(range.end)?;
            }
            _ => {}
        }
    }
    let code_start = code_start.context("WASM artifact has no code section")?;
    anyhow::ensure!(
        sections.contains_key(".debug_line") && sections.contains_key(".debug_info"),
        "WASM source maps require post-bindgen DWARF line tables; build with release debug=line-tables-only, strip=none and wasm-bindgen --keep-debug"
    );
    let dwarf: Dwarf<EndianSlice<'_, LittleEndian>> =
        Dwarf::load(|section: SectionId| -> Result<_, gimli::Error> {
            Ok(EndianSlice::new(
                sections.get(section.name()).copied().unwrap_or_default(),
                LittleEndian,
            ))
        })?;
    let mut units = dwarf.units();
    let mut locations = BTreeMap::new();
    while let Some(header) = units.next()? {
        let unit = dwarf.unit(header)?;
        let Some(program) = unit.line_program.clone() else {
            continue;
        };
        let compilation = unit
            .comp_dir
            .as_ref()
            .map(|directory| PathBuf::from(directory.to_string_lossy().as_ref()))
            .unwrap_or_default();
        let mut rows = program.rows();
        while let Some((header, row)) = rows.next_row()? {
            if row.end_sequence() {
                continue;
            }
            let Some(file) = row.file(header) else {
                continue;
            };
            let Some(line) = row.line() else {
                continue;
            };
            let name = dwarf
                .attr_string(&unit, file.path_name())?
                .to_string_lossy()
                .into_owned();
            let directory = file
                .directory(header)
                .map(|directory| dwarf.attr_string(&unit, directory))
                .transpose()?
                .map(|directory| PathBuf::from(directory.to_string_lossy().as_ref()))
                .unwrap_or_default();
            let file = compilation.join(directory).join(name).clean();
            let Some(offset) = row.address().checked_add(code_start) else {
                continue;
            };
            if offset >= code_end {
                continue;
            }
            let column = match row.column() {
                gimli::ColumnType::LeftEdge => 0,
                gimli::ColumnType::Column(column) => column.get() - 1,
            };
            locations.insert(
                offset,
                Location {
                    offset,
                    file,
                    line: line.get() - 1,
                    column,
                },
            );
        }
    }
    anyhow::ensure!(
        !locations.is_empty(),
        "WASM DWARF contains no executable source locations"
    );
    Ok(locations.into_values().collect())
}

fn source_url(file: &Path, repository: &Path) -> String {
    if let Ok(relative) = file.strip_prefix(repository) {
        format!(
            "../../../{}",
            relative
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/")
        )
    } else {
        file.to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/")
    }
}

fn vlq(value: i64, output: &mut String) {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = (value.unsigned_abs() << 1) | u64::from(value < 0);
    loop {
        let digit = (encoded & 31) | if encoded >> 5 == 0 { 0 } else { 32 };
        output.push(char::from(
            ALPHABET[usize::try_from(digit).expect("VLQ digit is at most 63")],
        ));
        encoded >>= 5;
        if encoded == 0 {
            break;
        }
    }
}

/// Map executable WASM byte offsets to their Rust source locations.
///
/// # Errors
/// Returns malformed-module, missing-DWARF, line-table, or position-overflow errors.
pub fn wasm_source_map(wasm: &[u8], file: &str, repository: &Path) -> anyhow::Result<SourceMap> {
    let locations = dwarf_locations(wasm)?;
    let mut indices = BTreeMap::new();
    let mut sources = Vec::new();
    let mut sources_content = Vec::new();
    let mut mappings = String::new();
    let (mut prior_offset, mut prior_source, mut prior_line, mut prior_column) =
        (0_i64, 0_i64, 0_i64, 0_i64);
    for location in locations {
        let index = if let Some(index) = indices.get(&location.file) {
            *index
        } else {
            let index = i64::try_from(sources.len())?;
            sources.push(source_url(&location.file, repository));
            sources_content.push(std::fs::read_to_string(&location.file).ok());
            indices.insert(location.file.clone(), index);
            index
        };
        if !mappings.is_empty() {
            mappings.push(',');
        }
        let offset = i64::try_from(location.offset)?;
        let line = i64::try_from(location.line)?;
        let column = i64::try_from(location.column)?;
        for delta in [
            offset - prior_offset,
            index - prior_source,
            line - prior_line,
            column - prior_column,
        ] {
            vlq(delta, &mut mappings);
        }
        (prior_offset, prior_source, prior_line, prior_column) = (offset, index, line, column);
    }
    Ok(SourceMap {
        version: 3,
        file: file.to_owned(),
        sources,
        sources_content,
        names: Vec::new(),
        mappings,
    })
}

fn unsigned_leb(mut value: usize, output: &mut Vec<u8>) {
    loop {
        let byte = u8::try_from(value & 127).expect("LEB digit fits a byte");
        value >>= 7;
        output.push(byte | if value == 0 { 0 } else { 128 });
        if value == 0 {
            break;
        }
    }
}

fn remove_source_map_url(wasm: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut result = wasm.get(..8).context("WASM header is truncated")?.to_vec();
    let mut previous = 8;
    for payload in Parser::new(0).parse_all(wasm) {
        let payload = payload?;
        if let Some((_, range)) = payload.as_section() {
            if !matches!(&payload, Payload::CustomSection(section) if section.name() == "sourceMappingURL")
            {
                result.extend_from_slice(&wasm[previous..range.end]);
            }
            previous = range.end;
        }
    }
    anyhow::ensure!(
        previous == wasm.len(),
        "WASM map patch omitted trailing module bytes"
    );
    Ok(result)
}

/// Attach the standard URL custom section, replacing a previous URL entry.
///
/// # Errors
/// Returns an error for malformed or truncated WASM bytes.
pub fn attach_source_map_url(wasm: &[u8], url: &str) -> anyhow::Result<Vec<u8>> {
    let mut wasm = remove_source_map_url(wasm)?;
    let mut payload = Vec::new();
    unsigned_leb("sourceMappingURL".len(), &mut payload);
    payload.extend_from_slice(b"sourceMappingURL");
    unsigned_leb(url.len(), &mut payload);
    payload.extend_from_slice(url.as_bytes());
    wasm.push(0);
    unsigned_leb(payload.len(), &mut wasm);
    wasm.extend(payload);
    Ok(wasm)
}

/// Drop the DWARF custom sections a browser never reads.
///
/// The line tables stay available to debugging through the map written beside the artifact, so
/// the shipped module keeps its `sourceMappingURL`, its `name` section, and every content
/// section byte-for-byte; only `.debug_*` payloads are removed.
///
/// # Errors
///
/// Returns an error for malformed or truncated WASM bytes.
pub fn strip_debug_sections(wasm: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut result = wasm.get(..8).context("WASM header is truncated")?.to_vec();
    let mut previous = 8;
    for payload in Parser::new(0).parse_all(wasm) {
        let payload = payload?;
        if let Some((_, range)) = payload.as_section() {
            let debug = matches!(
                &payload,
                Payload::CustomSection(section) if section.name().starts_with(".debug_")
            );
            if !debug {
                result.extend_from_slice(&wasm[previous..range.end]);
            }
            previous = range.end;
        }
    }
    anyhow::ensure!(
        previous == wasm.len(),
        "WASM debug strip omitted trailing module bytes"
    );
    Ok(result)
}

/// Write a post-bindgen WASM map beside its artifact, attach its browser URL, and ship the module
/// without the DWARF the map already carries.
///
/// # Errors
/// Returns map-generation errors or filesystem read/write failures.
pub fn write_wasm_source_map(wasm: &Path, repository: &Path) -> anyhow::Result<SourceMap> {
    let file = wasm
        .file_name()
        .context("WASM artifact has no file name")?
        .to_string_lossy();
    let bytes = remove_source_map_url(&std::fs::read(wasm)?)?;
    let map = wasm_source_map(&bytes, &file, repository)?;
    let map_name = format!("{file}.map");
    std::fs::write(wasm.with_file_name(&map_name), serde_json::to_vec(&map)?)?;
    std::fs::write(
        wasm,
        strip_debug_sections(&attach_source_map_url(&bytes, &map_name)?)?,
    )?;
    Ok(map)
}

/// Map the unchanged binding prefix of a generated plugin script to its embedded binding source.
///
/// # Errors
/// Returns an error if packaging changed binding lines or omitted the binding prefix.
pub fn binding_source_map(
    file: &str,
    bindings: &str,
    script: &str,
    source_url: &str,
) -> anyhow::Result<SourceMap> {
    let binding_lines = bindings.lines().collect::<Vec<_>>();
    let script_lines = script.lines().collect::<Vec<_>>();
    anyhow::ensure!(
        binding_lines.len() <= script_lines.len(),
        "plugin script is shorter than its WASM bindings"
    );
    let mut mappings = String::new();
    for (index, line) in binding_lines.iter().enumerate() {
        if index > 0 {
            anyhow::ensure!(
                script_lines[index] == *line,
                "plugin binding source differs at line {}",
                index + 1
            );
            mappings.push(';');
        }
        // Only the first line's declaration may be renamed to the package's unique global.
        mappings.push_str(if index == 0 { "AAAA" } else { "AACA" });
    }
    Ok(SourceMap {
        version: 3,
        file: file.to_owned(),
        sources: vec![source_url.to_owned()],
        sources_content: vec![Some(bindings.to_owned())],
        names: Vec::new(),
        mappings,
    })
}

/// Attach the binding map before any dependency bundler transforms the script.
///
/// # Errors
/// Returns binding-position validation or map serialization errors.
pub fn annotate_binding_script(
    bindings: &str,
    script: &str,
    source_url: &str,
) -> anyhow::Result<String> {
    let map = binding_source_map("client.js", bindings, script, source_url)?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&map)?);
    Ok(format!(
        "{script}\n//# sourceMappingURL=data:application/json;base64,{encoded}\n"
    ))
}

/// Publish a bundled script and the source map carried through its compiler pipeline.
///
/// # Errors
/// Returns an error for a missing or invalid inline map or an output write failure.
pub fn write_binding_artifact(path: &Path, script: &str) -> anyhow::Result<()> {
    let (offset, line) = script
        .match_indices("//# sourceMappingURL=data:application/json")
        .filter(|(offset, _)| *offset == 0 || script.as_bytes()[offset - 1] == b'\n')
        .last()
        .map(|(offset, _)| (offset, script[offset..].lines().next().unwrap_or_default()))
        .context("Client dependency compiler omitted its source map")?;
    let encoded = line
        .split_once("base64,")
        .context("Client source map is not base64 encoded")?
        .1;
    let mut map: serde_json::Value =
        serde_json::from_slice(&base64::engine::general_purpose::STANDARD.decode(encoded)?)?;
    anyhow::ensure!(
        map.get("version").and_then(serde_json::Value::as_u64) == Some(3)
            && map.get("sources").is_some_and(serde_json::Value::is_array)
            && map
                .get("mappings")
                .is_some_and(serde_json::Value::is_string),
        "Client dependency compiler emitted an invalid Source Map v3 document"
    );
    let file = path
        .file_name()
        .context("Client artifact has no file name")?
        .to_string_lossy();
    let served_file = if file == "client.web.js" {
        "client.js"
    } else {
        &file
    };
    map["file"] = serde_json::json!(served_file);
    let map_name = format!("{file}.map");
    std::fs::write(path.with_file_name(map_name), serde_json::to_vec(&map)?)?;
    let end = offset + line.len();
    let script = format!(
        "{}//# sourceMappingURL={served_file}.map{}",
        &script[..offset],
        &script[end..]
    );
    std::fs::write(path, script)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wasm_map_url_is_replaced_without_duplicating_sections() {
        let empty = b"\0asm\x01\0\0\0";
        let first = attach_source_map_url(empty, "first.map").unwrap();
        let second = attach_source_map_url(&first, "second.map").unwrap();
        let names: Vec<_> = Parser::new(0)
            .parse_all(&second)
            .filter_map(|payload| match payload.unwrap() {
                Payload::CustomSection(section) => {
                    Some((section.name().to_owned(), section.data().to_vec()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            names,
            [("sourceMappingURL".to_owned(), b"\x0asecond.map".to_vec())]
        );
        assert_eq!(remove_source_map_url(&second).unwrap(), empty);
    }

    #[test]
    fn shipped_modules_drop_dwarf_and_keep_every_other_section() {
        fn custom_section(name: &str, data: &[u8]) -> Vec<u8> {
            let mut payload = Vec::new();
            unsigned_leb(name.len(), &mut payload);
            payload.extend_from_slice(name.as_bytes());
            payload.extend_from_slice(data);
            let mut section = vec![0];
            unsigned_leb(payload.len(), &mut section);
            section.extend(payload);
            section
        }

        let mut module = b"\0asm\x01\0\0\0".to_vec();
        module.extend_from_slice(&custom_section("name", b"\x01\x02\x03"));
        let mut with_debug = module.clone();
        with_debug.extend_from_slice(&custom_section(".debug_info", &[7; 4096]));
        with_debug.extend_from_slice(&custom_section(".debug_str", &[9; 2048]));
        let mapped = attach_source_map_url(&with_debug, "client_bg.wasm.map").unwrap();

        let stripped = strip_debug_sections(&mapped).unwrap();
        assert_eq!(
            stripped,
            attach_source_map_url(&module, "client_bg.wasm.map").unwrap()
        );
        assert_eq!(strip_debug_sections(&stripped).unwrap(), stripped);
        assert_eq!(
            strip_debug_sections(b"\0asm\x01\0\0\0").unwrap(),
            b"\0asm\x01\0\0\0"
        );
        assert!(strip_debug_sections(b"\0asm\x01").is_err());
    }

    #[test]
    fn binding_maps_do_not_claim_locations_for_unmapped_wasm_literals() {
        let binding = "let wasm_bindgen = {};\nfunction call() {}\n";
        let script =
            "var plugin = {};\nfunction call() {}\nconst bytes = 'large encoded module';\n";
        let map = binding_source_map("client.js", binding, script, "bindings.js").unwrap();
        assert_eq!(map.mappings, "AAAA;AACA");
        assert_eq!(map.sources_content, [Some(binding.to_owned())]);
        assert!(
            binding_source_map("client.js", binding, "changed\nwrong\n", "bindings.js").is_err()
        );
    }

    #[test]
    fn final_binding_maps_follow_host_aliases_and_reject_malformed_compiler_output() {
        let directory = tempfile::tempdir().unwrap();
        let binding = "let wasm_bindgen = {};\nfunction call() {}\n";
        let annotated = annotate_binding_script(binding, binding, "bindings.js").unwrap();
        for (file, served) in [
            ("client.web.js", "client.js"),
            ("client.js", "client.js"),
            ("wasm.js", "wasm.js"),
        ] {
            let path = directory.path().join(file);
            write_binding_artifact(&path, &annotated).unwrap();
            assert!(
                std::fs::read_to_string(&path)
                    .unwrap()
                    .ends_with(&format!("//# sourceMappingURL={served}.map\n"))
            );
            let map: serde_json::Value = serde_json::from_slice(
                &std::fs::read(directory.path().join(format!("{file}.map"))).unwrap(),
            )
            .unwrap();
            assert_eq!(map["file"], served);
            assert_eq!(map["sourcesContent"][0], binding);
        }
        let invalid = "//# sourceMappingURL=data:application/json;base64,ZmFsc2U=\n";
        assert!(write_binding_artifact(&directory.path().join("invalid.js"), invalid).is_err());
        assert!(!directory.path().join("invalid.js").exists());
    }
}
