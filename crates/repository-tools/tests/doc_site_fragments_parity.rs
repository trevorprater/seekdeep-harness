//! Built-page inventory, routing, URL, fragment, and diagnostic fixtures.

use seekdeep_repository_tools::doc_site_fragments::{
    BrokenSiteFragment, SiteFragmentReport, inspect_site_fragments,
};
use tempfile::TempDir;

fn write(root: &TempDir, relative: &str, content: &str) {
    let path = root.path().join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn fixture() -> TempDir {
    let root = tempfile::tempdir().unwrap();
    write(
        &root,
        "index.html",
        "<a id=\"home\"></a><a href=\"/guide/start#ready\">start</a>",
    );
    write(
        &root,
        "guide/start.html",
        &[
            "<h1 id=\"ready\">Ready</h1>",
            "<a name=\"legacy\"></a>",
            "<a href=\"#ready\">same page</a>",
            "<a href=\"./start.html#legacy\">html alias</a>",
            "<a href=\"../#home\">root</a>",
            "<a href=\"https://example.com/page#missing\">external</a>",
        ]
        .join(""),
    );
    root
}

#[test]
fn rejects_a_directory_with_no_built_pages() {
    let root = tempfile::tempdir().unwrap();
    let error = inspect_site_fragments(root.path()).unwrap_err().to_string();
    assert!(error.contains("no HTML files found"));
}

#[test]
fn resolves_clean_encoded_and_same_page_routes() {
    let root = fixture();
    write(
        &root,
        "guide/encoded.html",
        "<h1 id=\"a b\">Encoded</h1><h2 id=\"%\">Literal</h2><a href=\"./encoded#a%20b\">encoded</a><a href=\"#%\">literal</a>",
    );
    assert_eq!(
        inspect_site_fragments(root.path()).unwrap(),
        SiteFragmentReport {
            checked: 6,
            broken: Vec::new(),
        }
    );
}

#[test]
fn rejects_ambiguous_built_routes() {
    let root = fixture();
    write(&root, "guide.html", "<h1 id=\"flat\">Flat</h1>");
    write(&root, "guide/index.html", "<h1 id=\"index\">Index</h1>");
    let error = inspect_site_fragments(root.path()).unwrap_err().to_string();
    assert!(error.contains("share route \"/guide\""), "{error}");
}

#[test]
fn rejects_malformed_fragment_hrefs() {
    let root = fixture();
    write(
        &root,
        "guide/invalid.html",
        "<a href=\"http://[invalid]#fragment\">invalid</a>",
    );
    let error = inspect_site_fragments(root.path()).unwrap_err().to_string();
    assert!(
        error
            .contains("guide/invalid.html has invalid fragment href \"http://[invalid]#fragment\""),
        "{error}"
    );
}

#[test]
fn reports_missing_ids_and_missing_built_routes() {
    let root = fixture();
    write(
        &root,
        "guide/broken.html",
        "<a href=\"./start#missing\">id</a><a href=\"./absent#missing\">route</a>",
    );
    assert_eq!(
        inspect_site_fragments(root.path()).unwrap().broken,
        [
            BrokenSiteFragment {
                source: "guide/broken.html".to_owned(),
                href: "./start#missing".to_owned(),
                target: Some("guide/start.html".to_owned()),
                fragment: "missing".to_owned(),
            },
            BrokenSiteFragment {
                source: "guide/broken.html".to_owned(),
                href: "./absent#missing".to_owned(),
                target: None,
                fragment: "missing".to_owned(),
            },
        ]
    );
}
