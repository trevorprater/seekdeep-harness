//! Native relationship graph generation, completeness checks, and freshness gate.

use std::{collections::HashSet, path::Path, sync::LazyLock};

use icu_collator::{Collator, CollatorBorrowed, CollatorPreferences, options::CollatorOptions};
use icu_locale::Locale;
use indexmap::{IndexMap, IndexSet};
use regex::Regex;
use seekdeep_typert_generator::{
    analyzer::{
        repository::TypeScriptProject,
        repository_graphs::{
            EventRelation, EventRelations, collect_event_relations, collect_package_sources,
        },
    },
    catalog::{CordisCatalogModel, EventEntry, ServiceEntry},
};
use serde::{Deserialize, Serialize};

use crate::package_graph::{
    PackageGraphNode, collect_package_graph, collect_package_graph_with_scope,
    escape_mermaid_label, graph_node_id,
};

/// One graph artifact, including its exact trailing newline.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GraphDoc {
    /// Repository-relative output path.
    pub rel: String,
    /// Fully rendered Markdown.
    pub content: String,
}

/// Reviewed classification of one discovered Cordis service.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ServiceRole {
    /// Cordis service key.
    pub key: String,
    /// Owning package short name.
    pub pkg: String,
    /// Human-readable service role.
    pub title: String,
    /// Core service, seam, or composition bundle.
    pub mode: String,
    /// Known concrete providers.
    #[serde(default)]
    pub implementations: Vec<String>,
    /// Direct consumer package short names.
    #[serde(default)]
    pub consumers: Vec<String>,
    /// Event-gate companion plugins.
    #[serde(default)]
    pub companions: Vec<String>,
    /// Policy explanation included in the graph table.
    pub note: String,
}

/// One application composition graph descriptor.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AppExample {
    /// Node namespace.
    pub id: String,
    /// Output artifact path.
    pub rel: String,
    /// Artifact title.
    pub title: String,
    /// Composition node label.
    pub label: String,
    /// Canonical Cordis configuration path.
    pub config: String,
    /// Composition explanation.
    pub summary: String,
}

/// Reviewed policy and curated diagrams consumed by the generator.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DocGraphPolicy {
    /// Topological-sort group preference.
    pub group_order: Vec<String>,
    /// Exhaustively classified Cordis services.
    pub service_roles: Vec<ServiceRole>,
    /// Application configs projected into composition graphs.
    pub app_examples: Vec<AppExample>,
    /// Curated lifecycle diagram.
    pub lifecycle: String,
    /// Curated tool execution diagram.
    pub tool_pipeline: String,
}

impl Default for DocGraphPolicy {
    fn default() -> Self {
        serde_json::from_str(include_str!("doc_graphs_policy.json")).expect("embedded graph policy")
    }
}

/// Check/write result for the complete generated artifact set.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct GraphCommandReport {
    /// Number of expected artifacts.
    pub documents: usize,
    /// Whether verification or writing succeeded.
    pub success: bool,
    /// Source-compatible standard output or error message.
    pub message: String,
    /// Stale or missing artifacts in generation order.
    pub stale: Vec<String>,
}

/// Repository-relative documentation directory for every package in a graph.
///
/// A graph's input is the pinned source's package list, so each node's own `relative`
/// path names a directory in that checkout. A ported package keeps its README either in
/// the retained `packages/<group>/<package>` directory or in the `crates/<package>`
/// crate that replaces it, so every link resolves against the repository the document is
/// written to rather than the checkout the graph was read from. Linking to the directory
/// that owns the README keeps one documented page per package, matching how the
/// repository's authored documentation links packages; a package with no README anywhere
/// stays a plain name instead of a dead link.
#[derive(Clone, Debug, Default)]
pub struct PackageLinks(IndexMap<String, String>);

impl PackageLinks {
    /// Resolves each package's documentation directory inside `repo_root`.
    #[must_use]
    pub fn resolve(repo_root: &Path, packages: &[PackageGraphNode]) -> Self {
        let directories = packages
            .iter()
            .filter_map(|package| {
                let directory = readme_directory(repo_root, &package.relative).or_else(|| {
                    readme_directory(repo_root, &format!("crates/{}", package.short))
                })?;
                Some((package.short.clone(), directory))
            })
            .collect();
        Self(directories)
    }

    fn get(&self, short: &str) -> Option<&str> {
        self.0.get(short).map(String::as_str)
    }
}

/// Returns `directory` or its target-identity rewrite, whichever owns a README.
///
/// The pinned source keeps its own identity in directory names, and the port renames them,
/// so a path can differ between the checkout the graph was read from and the repository the
/// document is written to. The written document renders the rewritten identity either way.
fn readme_directory(repo_root: &Path, directory: &str) -> Option<String> {
    let rewritten = target_identity(directory);
    if has_readme(repo_root, directory) {
        Some(directory.to_owned())
    } else if rewritten != directory && has_readme(repo_root, &rewritten) {
        Some(rewritten)
    } else {
        None
    }
}

fn has_readme(repo_root: &Path, directory: &str) -> bool {
    repo_root.join(directory).join("README.md").is_file()
}

/// Renders graphs from current manifests/configs and a semantic source program.
///
/// # Errors
/// Returns manifest, configuration, compiler, or completeness failures.
pub fn render_doc_graphs(
    repo_root: &Path,
    source_root: &Path,
    model: &CordisCatalogModel,
) -> anyhow::Result<Vec<GraphDoc>> {
    let policy = DocGraphPolicy::default();
    let packages = collect_package_graph(repo_root, &policy.group_order, "gen-doc-graphs")?;
    render_graph_set(repo_root, source_root, model, &packages, policy)
}

/// Renders the pinned source's complete graph inputs with target product identities.
///
/// `repo_root` is the repository the documents are written to, which owns the package
/// documentation directories every link resolves against; `source_root` is the pinned
/// checkout the graph inputs are read from.
///
/// # Errors
/// Returns manifest, configuration, compiler, or completeness failures.
pub fn render_source_doc_graphs(
    repo_root: &Path,
    source_root: &Path,
    model: &CordisCatalogModel,
) -> anyhow::Result<Vec<GraphDoc>> {
    let policy = DocGraphPolicy::default();
    let packages = collect_package_graph_with_scope(
        source_root,
        &policy.group_order,
        "gen-doc-graphs",
        "@deepseek-ai/dsh-",
    )?;
    render_graph_set(repo_root, source_root, model, &packages, policy)
}

fn render_graph_set(
    repo_root: &Path,
    source_root: &Path,
    model: &CordisCatalogModel,
    packages: &[PackageGraphNode],
    policy: DocGraphPolicy,
) -> anyhow::Result<Vec<GraphDoc>> {
    let directories = PackageLinks::resolve(repo_root, packages);
    let mut docs = vec![GraphDoc {
        rel: "docs/capability-seams.md".to_owned(),
        content: render_capability_seams(
            packages,
            &directories,
            &model.services,
            &policy.service_roles,
        )?,
    }];
    for example in &policy.app_examples {
        docs.push(GraphDoc {
            rel: example.rel.clone(),
            content: render_app_composition(source_root, example)?,
        });
    }
    let mut project = TypeScriptProject::new(source_root)?;
    let sources = collect_package_sources(&mut project)?;
    let relations = collect_event_relations(&mut project, &sources)?;
    docs.push(GraphDoc {
        rel: "docs/event-producer-consumer.md".to_owned(),
        content: render_event_relations(packages, &directories, &model.events, &relations)?,
    });
    docs.push(GraphDoc {
        rel: "docs/agent-lifecycle.md".to_owned(),
        content: policy.lifecycle,
    });
    docs.push(GraphDoc {
        rel: "docs/tool-execution-pipeline.md".to_owned(),
        content: policy.tool_pipeline,
    });
    docs.insert(
        0,
        GraphDoc {
            rel: "docs/graph-atlas.md".to_owned(),
            content: render_index(&docs),
        },
    );
    for doc in &mut docs {
        doc.content = target_identity(&doc.content);
    }
    Ok(docs)
}

/// Verifies exact output bytes or writes the complete generated artifact set.
///
/// # Errors
/// Returns file-read, directory-creation, or write failures.
pub fn write_or_check(
    root: &Path,
    docs: &[GraphDoc],
    check: bool,
) -> anyhow::Result<GraphCommandReport> {
    if check {
        let mut stale = Vec::new();
        for doc in docs {
            let path = root.join(&doc.rel);
            if !path.exists() || std::fs::read_to_string(path)? != doc.content {
                stale.push(doc.rel.clone());
            }
        }
        let success = stale.is_empty();
        let message = if success {
            format!(
                "gen-doc-graphs: {} graph doc(s) are up to date.\n",
                docs.len()
            )
        } else {
            format!(
                "gen-doc-graphs: stale graph doc(s): {}. Run `pnpm run gen-doc-graphs` and commit the result.\n",
                stale.join(", ")
            )
        };
        return Ok(GraphCommandReport {
            documents: docs.len(),
            success,
            message,
            stale,
        });
    }
    for doc in docs {
        let path = root.join(&doc.rel);
        std::fs::create_dir_all(
            path.parent()
                .ok_or_else(|| anyhow::anyhow!("graph output has no parent"))?,
        )?;
        std::fs::write(path, &doc.content)?;
    }
    Ok(GraphCommandReport {
        documents: docs.len(),
        success: true,
        message: format!("gen-doc-graphs: wrote {} graph doc(s).\n", docs.len()),
        stale: Vec::new(),
    })
}

/// Enforces both directions of the service-role allowlist.
///
/// # Errors
/// Reports all missing and stale service classifications.
pub fn assert_service_roles_complete(
    services: &[ServiceEntry],
    roles: &[ServiceRole],
) -> anyhow::Result<()> {
    let discovered = services
        .iter()
        .map(|service| service.key.as_str())
        .collect::<HashSet<_>>();
    let classified = roles
        .iter()
        .map(|role| role.key.as_str())
        .collect::<HashSet<_>>();
    let mut missing = discovered
        .difference(&classified)
        .copied()
        .collect::<Vec<_>>();
    let mut stale = classified
        .difference(&discovered)
        .copied()
        .collect::<Vec<_>>();
    sort_utf16(&mut missing);
    sort_utf16(&mut stale);
    let mut errors = Vec::new();
    if !missing.is_empty() {
        errors.push(format!(
            "missing service role classification: {}",
            missing.join(", ")
        ));
    }
    if !stale.is_empty() {
        errors.push(format!(
            "stale service role classification: {}",
            stale.join(", ")
        ));
    }
    if errors.is_empty() {
        Ok(())
    } else {
        anyhow::bail!(errors.join("; "))
    }
}

/// Renders declared service ownership and explicitly classified package roles.
///
/// # Errors
/// Returns missing or stale service-role classifications.
pub fn render_capability_seams(
    packages: &[PackageGraphNode],
    directories: &PackageLinks,
    services: &[ServiceEntry],
    roles: &[ServiceRole],
) -> anyhow::Result<String> {
    assert_service_roles_complete(services, roles)?;
    let by_short = packages
        .iter()
        .map(|package| (package.short.as_str(), package))
        .collect::<IndexMap<_, _>>();
    let mut lines = generated_header("Capability Seams And Core Services");
    lines.extend(["A service can be a core spine service, a swappable capability seam, or a bundle/composition point. The graph shows the package that owns the service declaration, known implementation packages, and packages that consume the service directly.", "", "```mermaid", "flowchart LR"].map(str::to_owned));
    let mut nodes = IndexMap::<String, String>::new();
    let mut edges = IndexSet::new();
    let mut companions = IndexSet::new();
    let mut node = |id: String, label: &str| {
        nodes
            .entry(id.clone())
            .or_insert_with(|| format!("  {id}[\"{}\"]", escape_mermaid_label(label)));
    };
    for role in roles {
        let service = graph_node_id("svc", &role.key);
        let owner = graph_node_id("pkg", &role.pkg);
        node(owner.clone(), &role.pkg);
        node(
            service.clone(),
            &format!("ctx.{}<br/>{}", role.key, role.title),
        );
        edges.insert(format!("  {owner} --> {service}"));
        for implementation in &role.implementations {
            let id = graph_node_id("pkg", implementation);
            node(id.clone(), implementation);
            edges.insert(format!("  {id} --> {service}"));
        }
        for consumer in &role.consumers {
            let id = graph_node_id("pkg", consumer);
            node(id.clone(), consumer);
            edges.insert(format!("  {service} --> {id}"));
        }
        for companion in &role.companions {
            let id = graph_node_id("pkg", companion);
            node(id.clone(), companion);
            companions.insert(format!("  {service} -. event gate .-> {id}"));
        }
    }
    lines.extend(nodes.into_values());
    let mut edges = edges.into_iter().collect::<Vec<_>>();
    let mut companions = companions.into_iter().collect::<Vec<_>>();
    sort_utf16(&mut edges);
    sort_utf16(&mut companions);
    lines.extend(edges);
    lines.extend(companions);
    lines.extend(["```", "", "| ctx key | Role | Owner | Implementations | Direct consumers | Companion plugins | Note |", "| --- | --- | --- | --- | --- | --- | --- |"].map(str::to_owned));
    for role in roles {
        lines.push(format!(
            "| `ctx.{}` | `{}` | {} | {} | {} | {} | {} |",
            role.key,
            role.mode,
            package_link(&by_short, directories, &role.pkg),
            package_list(&role.implementations, &by_short, directories),
            package_list(&role.consumers, &by_short, directories),
            package_list(&role.companions, &by_short, directories),
            role.note.replace('|', "\\|").replace('\n', "<br>")
        ));
    }
    lines.push(String::new());
    footer(
        &mut lines,
        "hybrid: services are discovered from Cordis declarations; interface/implementation/consumer roles are classified in `scripts/gen-doc-graphs.ts` with a completeness guard",
    );
    Ok(lines.join("\n"))
}

/// One parsed top-level or bundle-patch plugin row.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ExamplePlugin {
    /// Cordis row identity.
    pub id: String,
    /// Module or package name.
    pub name: String,
}

/// Parses the source generator's shallow `id`/`name` projection of Cordis YAML.
#[must_use]
pub fn parse_example_cordis(text: &str) -> Vec<ExamplePlugin> {
    static ID: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\s*-\s+id:\s+(.+?)\s*$").expect("composition id"));
    static NAME: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\s+name:\s+(.+?)\s*$").expect("composition name"));
    let mut plugins = Vec::new();
    let mut current: Option<ExamplePlugin> = None;
    for line in text.split('\n') {
        if let Some(capture) = ID.captures(line) {
            if let Some(plugin) = current.take().filter(|plugin| !plugin.name.is_empty()) {
                plugins.push(plugin);
            }
            current = Some(ExamplePlugin {
                id: strip_yaml_scalar(&capture[1]),
                name: String::new(),
            });
        } else if let Some(capture) = NAME.captures(line)
            && let Some(current) = &mut current
        {
            current.name = strip_yaml_scalar(&capture[1]);
        }
    }
    if let Some(plugin) = current.filter(|plugin| !plugin.name.is_empty()) {
        plugins.push(plugin);
    }
    plugins
}

/// Renders one application graph from its actual Cordis configuration.
///
/// # Errors
/// Returns configuration read failures.
pub fn render_app_composition(root: &Path, example: &AppExample) -> anyhow::Result<String> {
    let plugins = parse_example_cordis(&std::fs::read_to_string(root.join(&example.config))?);
    let mut lines = generated_header(&example.title);
    lines.extend([
        example.summary.clone(),
        String::new(),
        "```mermaid".to_owned(),
        "flowchart LR".to_owned(),
        format!(
            "  cfg[\"{}<br/>cordis.yml\"]",
            escape_mermaid_label(&example.label)
        ),
    ]);
    for plugin in &plugins {
        let id = graph_node_id(&format!("plugin_{}", example.id), &plugin.id);
        lines.push(format!(
            "  {id}[\"{}<br/>{}\"]",
            escape_mermaid_label(&plugin.id),
            escape_mermaid_label(&plugin.name)
        ));
        lines.push(format!("  cfg --> {id}"));
        if plugin.name == "@deepseek-ai/dsh-acp-demo"
            || plugin.name == "@seekdeep-ai/seekdeep-acp-demo"
        {
            app_expansion(&mut lines, &id);
        }
    }
    lines.extend(
        [
            "```",
            "",
            "| Plugin id | Package / module |",
            "| --- | --- |",
        ]
        .map(str::to_owned),
    );
    lines.extend(
        plugins
            .iter()
            .map(|plugin| format!("| `{}` | `{}` |", plugin.id, plugin.name)),
    );
    lines.push(String::new());
    lines.push(format!(
        "Source config: [`{}`]({}).",
        example.config,
        relative_path(
            Path::new(&example.rel).parent().unwrap_or(Path::new("")),
            Path::new(&example.config)
        )
    ));
    lines.push(String::new());
    footer(
        &mut lines,
        "hybrid: the leaf plugin list is parsed from its `cordis.yml`; app package expansion is curated from package source",
    );
    Ok(lines.join("\n"))
}

/// Renders semantic producer/listener edges and rejects undispatched Host events.
///
/// # Errors
/// Returns all declared Host events that lack a resolved dispatcher.
pub fn render_event_relations(
    packages: &[PackageGraphNode],
    directories: &PackageLinks,
    events: &[EventEntry],
    relations: &EventRelations,
) -> anyhow::Result<String> {
    let by_short = packages
        .iter()
        .map(|package| (package.short.as_str(), package))
        .collect::<IndexMap<_, _>>();
    let mut lines = generated_header("Event Producer And Consumer Matrix");
    lines.extend(["This matrix shows which packages dispatch each harness-owned event and which packages listen to it. Events are many-to-many, so the dense relation data is presented as a table rather than one large graph. Receiver and event-name types also cover contained dispatch sites that deliberately bypass `ctx.emit`, such as subagent lifecycle containment.", "", "| Event | Mode | Declared in | Dispatchers | Listeners |", "| --- | --- | --- | --- | --- |"].map(str::to_owned));
    let mut ordered = events.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| locale_compare(&left.name, &right.name));
    for event in ordered {
        let empty = EventRelation::default();
        let relation = relations.get(&event.name).unwrap_or(&empty);
        lines.push(format!(
            "| `{}` | `{}` | [`{}`](../{}) | {} | {} |",
            event.name,
            event.mode.as_str(),
            event.source,
            event.source.split(':').next().unwrap_or(&event.source),
            relation_packages(&relation.dispatchers, &by_short, directories),
            listener_packages(&relation.listeners, &by_short, directories)
        ));
    }
    let mut undispatched = events
        .iter()
        .filter(|event| !event.source.starts_with("packages/client/"))
        .filter(|event| {
            relations
                .get(&event.name)
                .is_none_or(|relation| relation.dispatchers.is_empty())
        })
        .map(|event| event.name.as_str())
        .collect::<Vec<_>>();
    sort_utf16(&mut undispatched);
    if !undispatched.is_empty() {
        anyhow::bail!(
            "event-producer-consumer matrix: no dispatcher found for declared event{} {} — dead vocabulary, or a dispatch form the semantic scan misses (teach scripts/gen-doc-graphs.ts that form)",
            if undispatched.len() > 1 { "s" } else { "" },
            undispatched
                .iter()
                .map(|name| format!("\"{name}\""))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let declared = events
        .iter()
        .map(|event| event.name.as_str())
        .collect::<HashSet<_>>();
    let mut extra = relations
        .keys()
        .filter(|event| !declared.contains(event.as_str()))
        .collect::<Vec<_>>();
    sort_utf16(&mut extra);
    if !extra.is_empty() {
        lines.extend(
            [
                "",
                "## Non-harness or undeclared event strings seen in package source",
                "",
                "| Event string | Dispatchers | Listeners |",
                "| --- | --- | --- |",
            ]
            .map(str::to_owned),
        );
        for event in extra {
            let relation = &relations[event];
            lines.push(format!(
                "| `{event}` | {} | {} |",
                relation_packages(&relation.dispatchers, &by_short, directories),
                listener_packages(&relation.listeners, &by_short, directories)
            ));
        }
    }
    lines.push(String::new());
    footer(
        &mut lines,
        "generated: Cordis event declarations and producer/listener edges are resolved from the repository TypeScript Program",
    );
    Ok(lines.join("\n"))
}

/// Renders the generated graph index in artifact order.
#[must_use]
pub fn render_index(docs: &[GraphDoc]) -> String {
    let mut lines = generated_header("Documentation Graph Index");
    lines.extend(["These diagrams show relationships that the generated catalogs do not. Use them to find package relationships, capability seams, event flow, model-facing tools, app composition, and runtime lifecycle paths. Exact signatures and type definitions still live in the [subsystem pages](subsystems/core.md) (types + the generated Cordis API regions) and [tool-catalog.md](tool-catalog.md).", "", "The process decision behind this index is recorded in [the documentation graph Agent Note](../.agents/notes/archived/process/2026-07-03-documentation-graph-atlas.md).", "", "| Graph | Mode |", "| --- | --- |", "| [module dependency graph](module-graph.md) | `generated` |", "| [tool schema catalog and package map](tool-catalog.md) | `generated` |"].map(str::to_owned));
    for doc in docs {
        let link = relative_path(Path::new("docs"), Path::new(&doc.rel));
        let (label, mode) = match doc.rel.as_str() {
            "docs/capability-seams.md" => {
                ("capability seams and core services", "hybrid generated")
            }
            "apps/cli/composition.md" => ("dsh shared base composition", "hybrid generated"),
            "examples/headless-agent/composition.md" => {
                ("headless-agent app composition", "hybrid generated")
            }
            "examples/cordis-agent/composition.md" => {
                ("cordis-agent app composition", "hybrid generated")
            }
            "examples/acp-agent/composition.md" => {
                ("acp-agent app composition", "hybrid generated")
            }
            "docs/event-producer-consumer.md" => {
                ("event producer/consumer matrix", "hybrid generated")
            }
            "docs/agent-lifecycle.md" => ("agent turn and step lifecycle", "curated"),
            "docs/tool-execution-pipeline.md" => ("tool execution pipeline", "curated"),
            _ => (link.as_str(), "generated"),
        };
        lines.push(format!("| [{label}]({link}) | `{mode}` |"));
    }
    lines.extend(["", "Regenerate with `pnpm run gen-doc-graphs`; verify freshness with `pnpm run verify-doc-graphs`.", ""].map(str::to_owned));
    footer(
        &mut lines,
        "mixed: each linked page declares generated, hybrid, or curated mode",
    );
    lines.join("\n")
}

/// Applies product/package identity renames to generated compatibility text.
#[must_use]
pub fn target_identity(text: &str) -> String {
    text.replace("DeepSeek Harness", "SeekDeep Harness")
        .replace("@deepseek-ai/", "@seekdeep-ai/")
        .replace("dsh-", "seekdeep-")
        .replace("dsh_", "seekdeep_")
        .replace("dsh shared", "seekdeep shared")
        .replace("DSH Base", "SeekDeep Base")
        .replace("DSH_*", "SEEKDEEP_*")
        .replace("__DSH_BOOT__", "__SEEKDEEP_BOOT__")
        .replace(
            "scripts/gen-doc-graphs.ts",
            "crates/repository-tools/src/doc_graphs.rs",
        )
}

fn generated_header(title: &str) -> Vec<String> {
    vec![
        "<!-- Generated by scripts/gen-doc-graphs.ts - do not edit by hand.".to_owned(),
        "     Run `pnpm run gen-doc-graphs` to regenerate. -->".to_owned(),
        String::new(),
        format!("# {title}"),
        String::new(),
    ]
}
fn footer(lines: &mut Vec<String>, mode: &str) {
    lines.extend([format!("Maintenance mode: {mode}."), String::new()]);
}
fn package_link(
    packages: &IndexMap<&str, &PackageGraphNode>,
    directories: &PackageLinks,
    name: &str,
) -> String {
    packages
        .get(name)
        .or_else(|| packages.get(target_identity(name).as_str()))
        .map_or_else(
            || format!("`{name}`"),
            |package| match directories.get(&package.short) {
                Some(directory) => format!("[`{}`](../{directory})", package.short),
                None => format!("`{}`", package.short),
            },
        )
}
fn package_list(
    names: &[String],
    packages: &IndexMap<&str, &PackageGraphNode>,
    directories: &PackageLinks,
) -> String {
    if names.is_empty() {
        "-".to_owned()
    } else {
        names
            .iter()
            .map(|name| package_link(packages, directories, name))
            .collect::<Vec<_>>()
            .join(", ")
    }
}
fn relation_packages(
    map: &IndexMap<String, IndexSet<String>>,
    packages: &IndexMap<&str, &PackageGraphNode>,
    directories: &PackageLinks,
) -> String {
    let mut ordered = map.iter().collect::<Vec<_>>();
    ordered.sort_by(|(left, _), (right, _)| locale_compare(left, right));
    let labels = ordered
        .into_iter()
        .map(|(package, methods)| {
            let mut methods = methods.iter().collect::<Vec<_>>();
            sort_utf16(&mut methods);
            format!(
                "{} ({})",
                package_link(packages, directories, package),
                methods
                    .into_iter()
                    .map(|method| format!("`{method}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
        .collect::<Vec<_>>();
    if labels.is_empty() {
        "-".to_owned()
    } else {
        labels.join(", ")
    }
}
fn listener_packages(
    names: &IndexSet<String>,
    packages: &IndexMap<&str, &PackageGraphNode>,
    directories: &PackageLinks,
) -> String {
    let mut names = names.iter().collect::<Vec<_>>();
    sort_utf16(&mut names);
    if names.is_empty() {
        "-".to_owned()
    } else {
        names
            .into_iter()
            .map(|name| package_link(packages, directories, name))
            .collect::<Vec<_>>()
            .join(", ")
    }
}
fn sort_utf16<T: AsRef<str>>(values: &mut [T]) {
    values.sort_by(|left, right| {
        left.as_ref()
            .encode_utf16()
            .cmp(right.as_ref().encode_utf16())
    });
}
fn strip_yaml_scalar(value: &str) -> String {
    let value = value.trim();
    let value = value.strip_prefix(['\'', '"']).unwrap_or(value);
    value.strip_suffix(['\'', '"']).unwrap_or(value).to_owned()
}
fn relative_path(from: &Path, to: &Path) -> String {
    let from = from.components().collect::<Vec<_>>();
    let to = to.components().collect::<Vec<_>>();
    let common = from
        .iter()
        .zip(&to)
        .take_while(|(left, right)| left == right)
        .count();
    let mut result = vec!["..".to_owned(); from.len() - common];
    result.extend(
        to[common..]
            .iter()
            .map(|part| part.as_os_str().to_string_lossy().into_owned()),
    );
    result.join("/")
}
fn locale_compare(left: &str, right: &str) -> std::cmp::Ordering {
    static COLLATOR: LazyLock<CollatorBorrowed<'static>> = LazyLock::new(|| {
        let locale = sys_locale::get_locale()
            .and_then(|locale| locale.parse::<Locale>().ok())
            .unwrap_or_else(|| "en-US".parse().expect("fallback locale"));
        Collator::try_new(
            CollatorPreferences::from(&locale),
            CollatorOptions::default(),
        )
        .expect("compiled collation data")
    });
    COLLATOR.compare(left, right)
}
fn app_expansion(lines: &mut Vec<String>, app: &str) {
    let core = graph_node_id("bundle", "agent_core");
    let jsonl = graph_node_id("bundle", "jsonl");
    lines.push(format!(
        "  {app} --> {core}[\"@deepseek-ai/dsh-agent-spine-demo\"]"
    ));
    lines.push(format!(
        "  {app} --> {jsonl}[\"@deepseek-ai/dsh-session-persistence-jsonl\"]"
    ));
    lines.push(format!("  {app} --> {}[\"@deepseek-ai/dsh-acp<br/>automation-only JSON-RPC stdio<br/>fresh sessions created by client\"]", graph_node_id("entrypoint", "acp")));
    for (name, label) in [
        ("llm", "ctx.llm"),
        ("sessions", "ctx.sessions"),
        ("tools", "ctx.tools + tool-bash"),
        ("loop", "ctx.agents + ctx.agentLoop"),
    ] {
        lines.push(format!(
            "  {core} --> {}[\"{label}\"]",
            graph_node_id("spine", name)
        ));
    }
}
