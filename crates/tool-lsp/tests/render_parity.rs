//! Pure tool rendering and coordinate-conversion parity.

use seekdeep_lsp::{LspHover, LspLocation, LspPosition, LspRange};
use seekdeep_tool_lsp::{
    DEFAULT_MAX_LOCATIONS, DEFAULT_MAX_RESULT_CHARS, LSP_OPERATIONS, LspToolArgs, format_hover,
    format_locations, parse_lsp_args, present_lsp_call, render_uri,
};
use seekdeep_tools::{FileLocation, GenericCallView, ToolCallKind, ToolCallView};

const WORKSPACE_URI: &str = "file:///home/u/proj";

fn location(uri: impl Into<String>, line: f64, character: f64) -> LspLocation {
    LspLocation {
        uri: uri.into(),
        range: LspRange {
            start: LspPosition { line, character },
            end: LspPosition {
                line,
                character: character + 1.0,
            },
        },
    }
}

fn args(operation: &str, file_path: &str, line: f64, character: f64) -> LspToolArgs {
    LspToolArgs {
        operation: operation.to_owned(),
        file_path: file_path.to_owned(),
        line,
        character,
    }
}

#[test]
fn arguments_accept_four_operations_and_reject_every_invalid_coordinate_or_path() {
    for operation in LSP_OPERATIONS {
        let input = parse_lsp_args(&args(operation, "a.ts", 3.0, 5.0)).unwrap();
        assert_eq!(input.operation.as_str(), operation);
        assert_eq!(
            input.position,
            LspPosition {
                line: 2.0,
                character: 4.0
            }
        );
    }
    assert!(
        parse_lsp_args(&args("rename", "a.ts", 1.0, 1.0))
            .unwrap_err()
            .to_string()
            .contains("operation must be one of")
    );
    assert!(
        parse_lsp_args(&args("hover", "   ", 1.0, 1.0))
            .unwrap_err()
            .to_string()
            .contains("file_path")
    );
    for (line, character, expected) in [
        (0.0, 1.0, "line"),
        (1.0, 0.0, "character"),
        (1.5, 1.0, "line"),
    ] {
        assert!(
            parse_lsp_args(&args("hover", "a.ts", line, character))
                .unwrap_err()
                .to_string()
                .contains(expected)
        );
    }
}

#[test]
fn uri_rendering_matches_posix_windows_remote_and_malformed_worlds() {
    for (uri, workspace, expected) in [
        ("file:///home/u/proj/src/a.ts", WORKSPACE_URI, "src/a.ts"),
        (
            "file:///home/u/other/lib/b.ts",
            WORKSPACE_URI,
            "/home/u/other/lib/b.ts",
        ),
        (WORKSPACE_URI, WORKSPACE_URI, "."),
        (
            "file:///home/u/proj/..generated/a.ts",
            WORKSPACE_URI,
            "..generated/a.ts",
        ),
        (
            "file:///C:/WORKSPACE/src/a.ts",
            "file:///c:/workspace",
            "src/a.ts",
        ),
        ("file:///D:/lib/b.ts", "file:///C:/workspace", "D:/lib/b.ts"),
        (
            "file://server/share/workspace/a.ts",
            "file://server/share/workspace",
            "a.ts",
        ),
        (
            "file://SERVER/share/workspace/src/A.ts",
            "file://server/Share/Workspace",
            "src/A.ts",
        ),
        (
            "file://other/share/b.ts",
            "file://server/share/workspace",
            "//other/share/b.ts",
        ),
        (
            "file:///D:/lib/a.ts",
            "file://server/share/workspace",
            "D:/lib/a.ts",
        ),
        ("file:///a.ts", "file://server/", "/a.ts"),
        ("file:///a.ts", "file:///", "a.ts"),
        (
            "file:///home/u/proj/dir%5Cname/a.ts",
            WORKSPACE_URI,
            "dir\\name/a.ts",
        ),
        ("file://[", WORKSPACE_URI, "file://["),
        (
            "file:///a.ts",
            "https://example.com/workspace",
            "file:///a.ts",
        ),
        ("file:///a.ts", "file:///bad%ZZ", "file:///a.ts"),
        (
            "file:///C:/workspace/bad%5Cpath",
            "file:///C:/workspace",
            "file:///C:/workspace/bad%5Cpath",
        ),
        ("file:///short", "file:///short/deeper", "/short"),
        ("file:///", "file:///C:/workspace", "/"),
        ("untitled:Untitled-1", WORKSPACE_URI, "untitled:Untitled-1"),
        (
            "jdt://contents/Foo.class",
            WORKSPACE_URI,
            "jdt://contents/Foo.class",
        ),
        ("file:///bad%2Fpath", WORKSPACE_URI, "file:///bad%2Fpath"),
        ("file:///bad%00path", WORKSPACE_URI, "file:///bad%00path"),
    ] {
        assert_eq!(render_uri(uri, workspace), expected, "{uri} in {workspace}");
    }
}

#[test]
fn location_and_hover_caps_include_exact_omission_and_truncation_markers() {
    assert_eq!(
        format_locations(
            &[],
            WORKSPACE_URI,
            DEFAULT_MAX_LOCATIONS,
            DEFAULT_MAX_RESULT_CHARS
        ),
        "No results."
    );
    let locations = [
        location("file:///home/u/proj/a.ts", 0.0, 0.0),
        location("file:///home/u/proj/a.ts", 4.0, 2.0),
    ];
    assert_eq!(
        format_locations(
            &locations,
            WORKSPACE_URI,
            DEFAULT_MAX_LOCATIONS,
            DEFAULT_MAX_RESULT_CHARS,
        ),
        "a.ts:1:1\na.ts:5:3"
    );
    let many = (0..5)
        .map(|line| location("file:///home/u/proj/a.ts", f64::from(line), 0.0))
        .collect::<Vec<_>>();
    let capped = format_locations(&many, WORKSPACE_URI, 2, DEFAULT_MAX_RESULT_CHARS);
    assert!(capped.contains("a.ts:1:1"));
    assert!(capped.contains("3 more locations omitted (limit 2)."));
    assert!(
        format_locations(&many[..2], WORKSPACE_URI, 1, DEFAULT_MAX_RESULT_CHARS)
            .contains("1 more location omitted (limit 1).")
    );
    let enormous = location(format!("custom:{}", "x".repeat(1_000_000)), 0.0, 0.0);
    let capped = format_locations(&[enormous], WORKSPACE_URI, 1, 80);
    assert_eq!(capped.encode_utf16().count(), 80);
    assert!(capped.contains("locations truncated"));

    assert_eq!(
        format_hover(None, DEFAULT_MAX_RESULT_CHARS),
        "No hover information."
    );
    let hover = LspHover {
        contents: "```ts\nx: number\n```".to_owned(),
        range: None,
    };
    assert_eq!(
        format_hover(Some(&hover), DEFAULT_MAX_RESULT_CHARS),
        hover.contents
    );
    let long = LspHover {
        contents: "a".repeat(100),
        range: None,
    };
    let capped = format_hover(Some(&long), 60);
    assert_eq!(capped.encode_utf16().count(), 60);
    assert!(capped.contains("hover truncated (limit 60 characters)."));
    assert_eq!(format_hover(Some(&long), 10).encode_utf16().count(), 10);
}

#[test]
fn pending_presentation_is_the_exact_generic_search_card() {
    assert_eq!(
        present_lsp_call(&args("findReferences", "a.ts", 3.0, 7.0)),
        ToolCallView::Generic(GenericCallView {
            title: "LSP findReferences a.ts:3:7".to_owned(),
            kind: Some(ToolCallKind::Search),
            raw_input: None,
            content: None,
            locations: Some(vec![FileLocation {
                path: "a.ts".to_owned(),
                line: Some(3.0),
            }]),
        })
    );
}
