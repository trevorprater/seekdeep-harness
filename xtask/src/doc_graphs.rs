//! Graph generation command using the shared Cordis catalog policy.

use std::path::Path;

use seekdeep_repository_tools::doc_graphs::{render_source_doc_graphs, write_or_check};
use seekdeep_typert_generator::{
    analyzer::run_with_stack, catalog::project_cordis_catalog, model::TypertFace,
};

/// Renders every graph and either writes it or verifies its exact contents.
///
/// # Errors
/// Returns compiler, configuration, manifest, graph completeness, or I/O failures.
pub fn run(root: &Path, source: &Path, check: bool) -> anyhow::Result<bool> {
    let output = std::fs::canonicalize(root)?;
    let source = std::fs::canonicalize(source)?;
    let docs = run_with_stack(move || -> anyhow::Result<_> {
        let projection = project_cordis_catalog(
            &source,
            &crate::cordis_catalog::cordis_catalog_policy(),
            TypertFace::Host,
        )?;
        render_source_doc_graphs(&source, &projection.model)
    })?;
    let report = write_or_check(&output, &docs, check)?;
    if report.success {
        print!("{}", report.message);
    } else {
        eprint!("{}", report.message);
    }
    Ok(report.success)
}

#[cfg(test)]
mod tests {
    use seekdeep_repository_tools::doc_graphs::{
        DocGraphPolicy, GraphDoc, assert_service_roles_complete, parse_example_cordis,
        render_doc_graphs, render_event_relations, target_identity,
    };
    use seekdeep_typert_generator::{
        analyzer::repository_graphs::EventRelations,
        catalog::{EventEntry, Mode, ServiceEntry},
    };

    use super::*;

    const SOURCE: &str = "/Users/trevor/ws/deepseek-harness";

    fn write(root: &Path, relative: &str, content: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn every_graph_matches_the_pinned_generated_artifact_and_round_trips_freshness() {
        run_with_stack(|| {
            let source = Path::new(SOURCE);
            let root = tempfile::tempdir().unwrap();
            for entry in walkdir::WalkDir::new(source.join("packages"))
                .min_depth(3)
                .max_depth(3)
            {
                let entry = entry.unwrap();
                if entry.file_name() != "package.json" {
                    continue;
                }
                let relative = entry.path().strip_prefix(source).unwrap().to_str().unwrap();
                write(
                    root.path(),
                    relative,
                    &target_identity(&std::fs::read_to_string(entry.path()).unwrap()),
                );
            }
            for example in DocGraphPolicy::default().app_examples {
                write(
                    root.path(),
                    &example.config,
                    &target_identity(
                        &std::fs::read_to_string(source.join(&example.config)).unwrap(),
                    ),
                );
            }
            let projection = project_cordis_catalog(
                source,
                &crate::cordis_catalog::cordis_catalog_policy(),
                TypertFace::Host,
            )
            .unwrap();
            let docs = render_doc_graphs(root.path(), source, &projection.model).unwrap();
            assert_eq!(docs.len(), 8);
            for doc in &docs {
                assert_eq!(
                    doc.content,
                    target_identity(&std::fs::read_to_string(source.join(&doc.rel)).unwrap()),
                    "{}",
                    doc.rel
                );
            }
            let missing = write_or_check(root.path(), &docs, true).unwrap();
            assert!(!missing.success);
            assert_eq!(
                missing.stale,
                docs.iter().map(|doc| doc.rel.clone()).collect::<Vec<_>>()
            );
            assert_eq!(
                write_or_check(root.path(), &docs, false).unwrap().message,
                "gen-doc-graphs: wrote 8 graph doc(s).\n"
            );
            assert_eq!(
                write_or_check(root.path(), &docs, true).unwrap().message,
                "gen-doc-graphs: 8 graph doc(s) are up to date.\n"
            );
            write(root.path(), &docs[2].rel, "stale\n");
            std::fs::remove_file(root.path().join(&docs[5].rel)).unwrap();
            let stale = write_or_check(root.path(), &docs, true).unwrap();
            assert!(!stale.success);
            assert_eq!(stale.stale, [docs[2].rel.clone(), docs[5].rel.clone()]);
            assert_eq!(
                stale.message,
                format!(
                    "gen-doc-graphs: stale graph doc(s): {}, {}. Run `pnpm run gen-doc-graphs` and commit the result.\n",
                    docs[2].rel, docs[5].rel
                )
            );
        });
    }

    #[test]
    fn classifications_and_dispatch_completeness_fail_closed_in_both_directions() {
        let mut policy = DocGraphPolicy::default();
        let mut services = policy
            .service_roles
            .iter()
            .map(|role| ServiceEntry {
                key: role.key.clone(),
                type_name: String::new(),
                is_abstract: false,
                doc: String::new(),
                methods: Vec::new(),
                source: String::new(),
            })
            .collect::<Vec<_>>();
        assert_service_roles_complete(&services, &policy.service_roles).unwrap();
        services[0].key = "missing".to_owned();
        let stale = policy.service_roles[0].key.clone();
        let error = assert_service_roles_complete(&services, &policy.service_roles).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "missing service role classification: missing; stale service role classification: {stale}"
            )
        );
        policy.service_roles.remove(0);
        let event = |name: &str, source: &str| EventEntry {
            name: name.to_owned(),
            scope: String::new(),
            signature: String::new(),
            js_doc: String::new(),
            mode: Mode::Emit,
            doc: String::new(),
            source: source.to_owned(),
        };
        let client = event("client/view", "packages/client/view/src/index.ts:1");
        render_event_relations(&[], &[client], &EventRelations::new()).unwrap();
        let error = render_event_relations(
            &[],
            &[
                event("z/host", "packages/core/z/src/index.ts:1"),
                event("a/host", "packages/core/a/src/index.ts:1"),
            ],
            &EventRelations::new(),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("no dispatcher found for declared events \"a/host\", \"z/host\"")
        );
    }

    #[test]
    fn composition_projection_and_write_failures_preserve_source_behavior() {
        let plugins = parse_example_cordis(
            "plugins:\n  - id: 'first'\n    name: \"one\"\n    patch:\n      - id: child\n        name: two\n  - id: ignored\n  - id: last\n    name: three\r\n",
        );
        assert_eq!(
            plugins
                .iter()
                .map(|row| (row.id.as_str(), row.name.as_str()))
                .collect::<Vec<_>>(),
            [("first", "one"), ("child", "two"), ("last", "three")]
        );
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "occupied", "file");
        assert!(
            write_or_check(
                root.path(),
                &[GraphDoc {
                    rel: "occupied/graph.md".to_owned(),
                    content: "graph\n".to_owned()
                }],
                false
            )
            .is_err()
        );
    }

    #[test]
    fn command_reads_complete_source_inputs_when_output_workspace_npm_metadata_is_partial() {
        let root = tempfile::tempdir().unwrap();
        write(
            root.path(),
            "packages/core/consumer/package.json",
            r#"{"name":"@seekdeep-ai/seekdeep-consumer","peerDependencies":{"@seekdeep-ai/seekdeep-invariants":"workspace:^"}}"#,
        );
        assert!(run(root.path(), Path::new(SOURCE), false).unwrap());
        for rel in [
            "docs/graph-atlas.md",
            "docs/capability-seams.md",
            "docs/event-producer-consumer.md",
            "apps/cli/composition.md",
        ] {
            assert_eq!(
                std::fs::read_to_string(root.path().join(rel)).unwrap(),
                target_identity(&std::fs::read_to_string(Path::new(SOURCE).join(rel)).unwrap()),
                "{rel}"
            );
        }
    }
}
