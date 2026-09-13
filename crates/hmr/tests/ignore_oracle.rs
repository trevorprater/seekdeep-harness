//! Differential coverage of the exact matcher used by the source Host watcher.

use std::{
    io::Write,
    process::{Command, Stdio},
};

use seekdeep_hmr::ignore::IgnoreMatcher;
use serde_json::json;

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "The differential fixture keeps generated pattern and path cases together."
)]
fn default_picomatch_grammar_matches_the_pinned_source() {
    let mut patterns = vec![
        "*",
        ".*",
        "*.*",
        "*/*",
        "**",
        "**/*",
        "**/*.*",
        "**/.*",
        "***",
        "**/**",
        "**/**/**",
        "**/node_modules",
        "**/.*",
        "cache",
        "data",
        "*.rs",
        "**/*.rs",
        "src/**",
        "src/**/*",
        "src/**/test.rs",
        "./src/**/test.rs",
        "a/**/b/**/c",
        "!*.rs",
        "!!*.rs",
        "!!!*.rs",
        "!src/**",
        "**/!(*.test).rs",
        "**/!(*.d).{ts,tsx}",
        "!(foo|bar)",
        "!(*-dbg).@(js)",
        "?(a|b)",
        "+(a|b)",
        "*(a|b)",
        "@(a|b)",
        "a?(b)c",
        "a+(b)c",
        "a*(b)c",
        "a@(b)c",
        "+(a|aa)",
        "*(*(a))",
        "+(*(a)*(b))",
        "*(+(a))",
        "+(a|)",
        "{a,b}",
        "{a,{b,c}}",
        "file{1..3}.js",
        "file{3..1}.js",
        "{a..c}",
        "{1..10}",
        "{1..5..2}",
        "{literal}",
        "{a,b",
        "a}",
        "[abc]",
        "[a-z]",
        "[^a]",
        "[!a]",
        "[[:digit:]]",
        "[[:alpha:]]",
        "[[:space:]]",
        "[[:punct:]]",
        "[[:word:]]",
        "[a-]",
        "[[]",
        "[]]",
        "[",
        "]",
        "[^]",
        "[z-a]",
        "a?",
        "?a",
        "??",
        "a*b",
        "a**b",
        "a***b",
        "a.*",
        "a.**",
        "a/*/b",
        "a/**b",
        "a/**/b",
        "a/**/**/b",
        "a/**/",
        "./*",
        "./**",
        "./*.rs",
        "a(b|c)",
        "a(b|c)+",
        "a(b|c)*",
        "a(b|c)?",
        "a(?=b)b",
        "a(?!b)c",
        "a(?:b)c",
        "a(",
        "a)",
        "a|b",
        "a+b",
        "a.b",
        "a$",
        "a^",
        "a\\?b",
        "a\\*b",
        "a\\.b",
        "a\\;b",
        "a\\/b",
        "a\\\\b",
        "a\\\\\\b",
        "a\\",
        "\"a*b\"",
        "src/\"a*b\"",
        "a b",
        "a  b",
        "a-b",
        "a--b",
        "a\0b",
        "文件/*.rs",
        "?😀",
        "a\nb",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    for prefix in ["", "a", "a/", "**/"] {
        for body in [
            "*",
            "?",
            "[ab]",
            "[^b]",
            "[[:digit:]]",
            "{a,b}",
            "(a|b)",
            "!(a|b)",
            "+(a|b)",
            "*(a|b)",
            "?(a|b)",
            "@(a|b)",
            "**",
            "\"a*b\"",
            "[a-]",
            "{a..c}",
            "+(aa|a)",
            "a\\?b",
        ] {
            for suffix in ["", "*", "?", "/", "/**", "b", ".js"] {
                patterns.push(format!("{prefix}{body}{suffix}"));
            }
        }
    }
    let mut paths = vec![
        "",
        ".",
        "..",
        "/",
        "a",
        "b",
        "c",
        "ab",
        "abc",
        "aa",
        "aaa",
        "aaaa",
        "aba",
        "abab",
        "aab",
        "bbb",
        "bc",
        "ac",
        "abbc",
        "a.c",
        "a.rs",
        ".rs",
        "a.test.rs",
        "a.d.ts",
        "a.ts",
        "a.tsx",
        "a-dbg.js",
        "a.js",
        "a/b",
        "a/b/c",
        "a/b.rs",
        "a/b/test.rs",
        "a/.b",
        ".a/b",
        "./a",
        "../a",
        "a/",
        "a//b",
        "src/",
        "src/a",
        "src/a.rs",
        "src/a/test.rs",
        "src/test.rs",
        "src/.cache/test.rs",
        "src/a/b/test.rs",
        "src/a*b",
        "a*b",
        "a?b",
        "a.b",
        "a;b",
        "a\\b",
        "a\\\\b",
        "a\nb",
        "a b",
        "a  b",
        "node_modules",
        "pkg/node_modules",
        "node_modules-copy",
        "cache",
        "cache/file",
        "data",
        "database",
        "pkg/data",
        ".git",
        "pkg/.cache",
        "foo",
        "bar",
        "foobar",
        "[abc]",
        "[!a]",
        "[",
        "]",
        "[a-]",
        "1",
        "2",
        "3",
        "5",
        "9",
        "file1.js",
        "file2.js",
        "file3.js",
        "file10.js",
        "{literal}",
        "{a,b",
        "a}",
        "a(",
        "a)",
        "a+b",
        "a-b",
        "a--b",
        "a$",
        "a^",
        "文件/main.rs",
        "中😀",
        "😀",
        "a\tb",
        " ",
        "\n",
        "!",
        "_",
        "Z",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    paths.extend(patterns.iter().cloned());
    assert_source_matches(&patterns, &paths);
}

fn assert_source_matches(patterns: &[String], paths: &[String]) {
    let source_root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../deepseek-harness");
    let mut child = Command::new("node")
        .args(["--input-type=module", "-e", "import { createRequire } from 'node:module'; import fs from 'node:fs'; const require=createRequire(process.cwd()+'/vendor/hmr/package.json'); const picomatch=require('picomatch'); const input=JSON.parse(fs.readFileSync(0,'utf8')); process.stdout.write(JSON.stringify(input.patterns.map(pattern=>({pattern,matches:input.paths.map(path=>picomatch(pattern)(path))}))));"])
        .current_dir(source_root)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("start source matcher");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            json!({"patterns":patterns,"paths":paths})
                .to_string()
                .as_bytes(),
        )
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    let mut failures = Vec::new();
    for row in rows {
        let pattern = row["pattern"].as_str().unwrap();
        let matcher = IgnoreMatcher::new(&[pattern.to_owned()]).unwrap();
        for (index, path) in paths.iter().enumerate() {
            let expected = row["matches"][index].as_bool().unwrap();
            if matcher.is_match(path) != expected {
                failures.push(format!(
                    "pattern={pattern:?} path={path:?} expected={expected} source={}",
                    row["source"]
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn pattern_validation_and_array_alternatives_preserve_source_behavior() {
    assert!(!IgnoreMatcher::new(&[]).unwrap().is_match("path"));
    assert!(
        IgnoreMatcher::new(&[String::new()])
            .unwrap_err()
            .to_string()
            .contains("non-empty string")
    );
    assert!(
        IgnoreMatcher::new(&["a".repeat(65_537)])
            .unwrap_err()
            .to_string()
            .contains("65537")
    );
    assert!(
        IgnoreMatcher::new(&["😀".repeat(32_769)])
            .unwrap_err()
            .to_string()
            .contains("65538")
    );
    let matcher = IgnoreMatcher::new(&["*.rs".into(), "!*.ts".into()]).unwrap();
    assert!(matcher.is_match("a.rs"));
    assert!(matcher.is_match("a.js"));
    assert!(!matcher.is_match("a.ts"));
}
