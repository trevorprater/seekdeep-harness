//! Source-compatible VLQ, bucketing, formatting, and UTF-16 fixtures.

#![allow(clippy::float_cmp)]

use seekdeep_chunk_attribution::{
    Attribution, BucketBytes, SourceMap, attribute_chunk, render_report,
};

#[test]
fn vlq_spans_bucket_and_render_like_the_source_script() {
    let map = SourceMap {
        sources: vec![
            "node_modules/react/index.js".to_owned(),
            "packages/client/web/src/index.ts".to_owned(),
        ],
        mappings: "AAAA;ACAA".to_owned(),
    };
    let result = attribute_chunk("abc\nxy", &map).unwrap();
    assert_eq!(result.total, 6.0);
    assert_eq!(result.rows[0].name, "react");
    assert_eq!(result.rows[0].bytes, 4.0);
    assert_eq!(result.rows[1].name, "ws:packages/client/web");
    assert_eq!(result.rows[1].bytes, 3.0);
    assert_eq!(result.rows[2].name, "(unmapped: interop glue/helpers)");
    assert_eq!(result.rows[2].bytes, 0.0);
    assert_eq!(
        render_report("dist/chunk.js", &result, 2.0),
        concat!(
            "chunk: dist/chunk.js  total 0.0 kB (minified, pre-gzip)\n",
            "      kB     %  package\n",
            "     0.0  66.7  react\n",
            "     0.0  50.0  ws:packages/client/web\n",
            "   ... 1 more buckets\n",
            "accounted: 0.0 kB of 0.0 kB\n",
            "\nGROUPS  npm-vendor 0.0 kB | workspace 0.0 kB | glue 0.0 kB\n",
        )
    );
    assert_eq!(
        render_report("dist/chunk.js", &result, f64::NAN),
        concat!(
            "chunk: dist/chunk.js  total 0.0 kB (minified, pre-gzip)\n",
            "      kB     %  package\n",
            "accounted: 0.0 kB of 0.0 kB\n",
            "\nGROUPS  npm-vendor 0.0 kB | workspace 0.0 kB | glue 0.0 kB\n",
        )
    );
}

#[test]
fn nested_node_modules_virtual_vendor_web_and_unmapped_segments_are_exact() {
    let map = SourceMap {
        sources: vec![
            "node_modules/a/node_modules/@scope/pkg/x.js".to_owned(),
            "\0virtual:vite/helper".to_owned(),
            "vendor/cordis/src/index.ts".to_owned(),
            "apps/web/src/main.ts".to_owned(),
        ],
        mappings: "AAAA,CCAA,CCAA,CCAA,C".to_owned(),
    };
    let result = attribute_chunk("abcdefghij", &map).unwrap();
    let names = result
        .rows
        .iter()
        .map(|row| row.name.as_str())
        .collect::<Vec<_>>();
    for expected in [
        "@scope/pkg",
        "(vite virtual/helpers)",
        "ws:vendor/cordis",
        "ws:apps/web",
        "(unmapped: interop glue/helpers)",
    ] {
        assert!(names.contains(&expected), "missing {expected}");
    }
}

#[test]
fn unicode_length_is_javascript_utf16_not_utf8() {
    let map = SourceMap {
        sources: vec!["source.ts".to_owned()],
        mappings: "AAAA".to_owned(),
    };
    let result = attribute_chunk("中😀", &map).unwrap();
    assert_eq!(result.total, 3.0);
    // The source algorithm attributes a synthetic newline to every mapped line.
    assert_eq!(result.rows[0].bytes, 4.0);
}

#[test]
fn decimal_half_rounding_matches_javascript_to_fixed() {
    let report = render_report(
        "chunk",
        &Attribution {
            total: 1024.0,
            rows: vec![BucketBytes {
                name: "half".to_owned(),
                bytes: 256.0,
            }],
        },
        1.0,
    );
    assert!(report.contains("     0.3  25.0  half"));
}
