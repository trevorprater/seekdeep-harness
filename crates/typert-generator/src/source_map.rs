//! Declaration mappings matching the source's mapped property-key spans.

use serde_json::json;

use crate::{Result, TypertGeneratorError};

#[derive(Default)]
pub(crate) struct DeclarationMap {
    names: Vec<String>,
    sources: Vec<String>,
    mappings: Vec<Mapping>,
}

struct Mapping {
    generated_line: usize,
    key_length: usize,
    source: usize,
    original_line: usize,
    original_column: usize,
    name: usize,
}

impl DeclarationMap {
    pub(crate) fn add(
        &mut self,
        generated_line: usize,
        key_length: usize,
        source: &str,
        original_line: usize,
        original_column: usize,
        name: &str,
    ) -> Result<()> {
        let source = index(&mut self.sources, source);
        let name = index(&mut self.names, name);
        self.mappings.push(Mapping {
            generated_line,
            key_length,
            source,
            original_line: original_line.checked_sub(1).ok_or_else(|| {
                TypertGeneratorError::Emit(
                    "Remote declaration source line must be positive".to_owned(),
                )
            })?,
            original_column: original_column.checked_sub(1).ok_or_else(|| {
                TypertGeneratorError::Emit(
                    "Remote declaration source column must be positive".to_owned(),
                )
            })?,
            name,
        });
        Ok(())
    }

    pub(crate) fn render(&self) -> String {
        let mut mappings = String::new();
        let mut line = 1;
        let mut prior_source = 0;
        let mut prior_line = 0;
        let mut prior_column = 0;
        let mut prior_name = 0;
        for mapping in &self.mappings {
            while line < mapping.generated_line {
                mappings.push(';');
                line += 1;
            }
            mappings.push_str(&vlq(4));
            mappings.push_str(&delta(mapping.source, &mut prior_source));
            mappings.push_str(&delta(mapping.original_line, &mut prior_line));
            mappings.push_str(&delta(mapping.original_column, &mut prior_column));
            mappings.push_str(&delta(mapping.name, &mut prior_name));
            mappings.push(',');
            mappings
                .push_str(&vlq(i64::try_from(mapping.key_length)
                    .expect("declaration fits source-map coordinates")));
        }
        let value = json!({
            "version": 3,
            "file": "typert.remote-client.d.ts",
            "names": self.names,
            "sources": self.sources,
            "sourcesContent": self.sources.iter().map(|_| serde_json::Value::Null).collect::<Vec<_>>(),
            "mappings": mappings,
            "ignoreList": [],
        });
        format!("{value}\n")
    }
}

fn index(values: &mut Vec<String>, value: &str) -> usize {
    if let Some(index) = values.iter().position(|candidate| candidate == value) {
        return index;
    }
    let index = values.len();
    values.push(value.to_owned());
    index
}

fn delta(value: usize, previous: &mut usize) -> String {
    let difference = i64::try_from(value).expect("source-map coordinate fits i64")
        - i64::try_from(*previous).expect("source-map coordinate fits i64");
    *previous = value;
    vlq(difference)
}

fn vlq(value: i64) -> String {
    const BASE64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut value = value.unsigned_abs() * 2 + u64::from(value < 0);
    let mut encoded = String::new();
    loop {
        let mut digit = usize::try_from(value & 31).expect("five-bit digit");
        value >>= 5;
        if value != 0 {
            digit |= 32;
        }
        encoded.push(char::from(BASE64[digit]));
        if value == 0 {
            break;
        }
    }
    encoded
}
