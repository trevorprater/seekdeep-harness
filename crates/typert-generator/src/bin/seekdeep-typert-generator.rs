//! JSON request runner for the Typert generator: one request on stdin, one reply on stdout.
//!
//! Compatibility bindings (the Vitest corpus adapter and the tsdown plugin
//! shim) drive every generator behavior through this executable so the
//! analysis, emission, and artifact publication stay in compiled Rust.

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    native::main();
}

#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::{io::Read as _, path::PathBuf};

    use seekdeep_typert_generator::{
        Result, TypertGeneratorError,
        analyzer::{WorkspaceAnalyzer, WorkspaceAnalyzerOptions, run_with_stack},
        catalog::{
            CordisCatalogModel, CordisCatalogPolicy, CordisCatalogProjector, EventEntry,
            ServiceEntry, project_cordis_catalog, render_inherited_page, render_page_region,
        },
        emitter::FaceModelEmitter,
        model::{
            FaceModel, MemberId, MemberModel, SourceDeclarationModel, SymbolId, TypeGraph,
            TypeNodeId, TypertFace,
        },
        renderer::TypeGraphRenderer,
        workspace::{
            WorkspaceEmitResult, WorkspaceTypertGenerator, has_typert_export, package_root,
            workspace_root, write_artifacts,
        },
    };
    use serde::Deserialize;
    use serde_json::{Value, json};

    #[derive(Deserialize)]
    #[serde(tag = "command", rename_all = "camelCase")]
    enum Request {
        #[serde(rename_all = "camelCase")]
        Analyze {
            options: WorkspaceAnalyzerOptions,
        },
        #[serde(rename_all = "camelCase")]
        AnalyzeInBatches {
            options: WorkspaceAnalyzerOptions,
            #[serde(default)]
            batch_size: Option<Value>,
        },
        DiscoverPackages {
            options: WorkspaceAnalyzerOptions,
        },
        IndexSourceDeclarations {
            options: WorkspaceAnalyzerOptions,
        },
        #[serde(rename_all = "camelCase")]
        Discover {
            root: PathBuf,
            #[serde(default)]
            faces: Option<Vec<TypertFace>>,
        },
        #[serde(rename_all = "camelCase")]
        Generate {
            root: PathBuf,
            #[serde(default)]
            packages: Option<Vec<String>>,
            #[serde(default)]
            faces: Option<Vec<TypertFace>>,
        },
        Emit {
            face: FaceModel,
            package: String,
        },
        #[serde(rename_all = "camelCase")]
        WriteArtifacts {
            package_dir: PathBuf,
            artifacts: Vec<WorkspaceEmitResult>,
        },
        WorkspaceRoot {
            start: PathBuf,
        },
        PackageRoot {
            start: PathBuf,
            workspace: PathBuf,
        },
        HasTypertExport {
            exports: Value,
        },
        #[serde(rename_all = "camelCase")]
        Transpile {
            code: String,
            file: String,
        },
        RenderType {
            graph: TypeGraph,
            id: TypeNodeId,
        },
        RenderDeclaration {
            graph: TypeGraph,
            id: SymbolId,
        },
        #[serde(rename_all = "camelCase")]
        ProjectCordisCatalog {
            root: PathBuf,
            policy: CordisCatalogPolicy,
            #[serde(default)]
            face: Option<TypertFace>,
        },
        #[serde(rename_all = "camelCase")]
        RenderRuntimeApi {
            face: FaceModel,
            source_declarations: Vec<SourceDeclarationModel>,
            policy: CordisCatalogPolicy,
            model: CordisCatalogModel,
        },
        RenderPageRegion {
            page: String,
            services: Vec<ServiceEntry>,
            events: Vec<EventEntry>,
            policy: CordisCatalogPolicy,
        },
        RenderInheritedPage {
            policy: CordisCatalogPolicy,
        },
        ChildTypeNodeIds {
            node: seekdeep_typert_generator::model::TypeNodeModel,
        },
        RendererNode {
            graph: TypeGraph,
            id: TypeNodeId,
        },
        RendererDeclaration {
            graph: TypeGraph,
            id: SymbolId,
        },
        RendererMember {
            graph: TypeGraph,
            id: MemberId,
        },
        RenderMember {
            graph: TypeGraph,
            member: MemberModel,
        },
        DeclarationClosureForMembers {
            graph: TypeGraph,
            members: Vec<MemberId>,
        },
        DeclarationClosureForTypes {
            graph: TypeGraph,
            types: Vec<TypeNodeId>,
        },
    }

    #[expect(
        clippy::too_many_lines,
        reason = "One dispatch arm per request keeps the wire protocol readable in one place"
    )]
    fn run(request: Request) -> Result<Value> {
        let encode = |value: &dyn erased::Encode| value.encode();
        match request {
            Request::Analyze { options } => {
                Ok(encode(&WorkspaceAnalyzer::new(options)?.analyze()?))
            }
            Request::AnalyzeInBatches {
                options,
                batch_size,
            } => {
                let size = match batch_size {
                    None => 8,
                    Some(value) => match value.as_u64() {
                        Some(size) if size >= 1 && value.is_u64() => {
                            usize::try_from(size).unwrap_or(usize::MAX)
                        }
                        _ => {
                            return Err(TypertGeneratorError::Analysis(format!(
                                "typert: batch size must be a positive integer, received {value}"
                            )));
                        }
                    },
                };
                Ok(encode(
                    &WorkspaceAnalyzer::new(options)?.analyze_in_batches(size)?,
                ))
            }
            Request::DiscoverPackages { options } => Ok(encode(
                &WorkspaceAnalyzer::new(options)?.discover_packages()?,
            )),
            Request::IndexSourceDeclarations { options } => Ok(encode(
                &WorkspaceAnalyzer::new(options)?.index_source_declarations()?,
            )),
            Request::Discover { root, faces } => Ok(encode(
                &WorkspaceTypertGenerator::new(root).discover(faces)?,
            )),
            Request::Generate {
                root,
                packages,
                faces,
            } => Ok(encode(
                &WorkspaceTypertGenerator::new(root).generate(packages, faces)?,
            )),
            Request::Emit { face, package } => {
                Ok(encode(&FaceModelEmitter::new(&face).emit(&package)?))
            }
            Request::WriteArtifacts {
                package_dir,
                artifacts,
            } => {
                write_artifacts(&package_dir, &artifacts)
                    .map_err(|error| TypertGeneratorError::Workspace(error.to_string()))?;
                Ok(Value::Null)
            }
            Request::WorkspaceRoot { start } => {
                Ok(json!(workspace_root(&start)?.to_string_lossy()))
            }
            Request::PackageRoot { start, workspace } => Ok(package_root(&start, &workspace)?
                .map_or(Value::Null, |root| json!(root.to_string_lossy()))),
            Request::HasTypertExport { exports } => Ok(json!(has_typert_export(&exports))),
            Request::Transpile { code, file } => Ok(seekdeep_typert_generator::plugin::transpile(
                &code, &file,
            )?
            .map_or(
                Value::Null,
                |output| json!({"code": output.code, "map": output.map}),
            )),
            Request::RenderType { graph, id } => Ok(json!(
                TypeGraphRenderer::new(&graph).render_type(&id, None)?
            )),
            Request::RenderDeclaration { graph, id } => Ok(json!(
                TypeGraphRenderer::new(&graph).render_declaration(&id)?
            )),
            Request::ProjectCordisCatalog { root, policy, face } => Ok(encode(
                &project_cordis_catalog(&root, &policy, face.unwrap_or(TypertFace::Host))?,
            )),
            Request::RenderRuntimeApi {
                face,
                source_declarations,
                policy,
                model,
            } => Ok(json!(
                CordisCatalogProjector::new(&face, &source_declarations, &policy)
                    .render_runtime_api(&model)?
            )),
            Request::RenderPageRegion {
                page,
                services,
                events,
                policy,
            } => Ok(json!(render_page_region(
                &page, &services, &events, &policy
            )?)),
            Request::RenderInheritedPage { policy } => Ok(json!(render_inherited_page(&policy))),
            Request::ChildTypeNodeIds { node } => Ok(json!(
                seekdeep_typert_generator::model::child_type_node_ids(&node)?
            )),
            Request::RendererNode { graph, id } => {
                Ok(encode(TypeGraphRenderer::new(&graph).node(&id)?))
            }
            Request::RendererDeclaration { graph, id } => {
                Ok(encode(TypeGraphRenderer::new(&graph).declaration(&id)?))
            }
            Request::RendererMember { graph, id } => {
                Ok(encode(TypeGraphRenderer::new(&graph).member(&id)?))
            }
            Request::RenderMember { graph, member } => Ok(json!(
                TypeGraphRenderer::new(&graph).render_member(&member, false, None)?
            )),
            Request::DeclarationClosureForMembers { graph, members } => Ok(encode(
                &TypeGraphRenderer::new(&graph).declaration_closure_for_members(&members)?,
            )),
            Request::DeclarationClosureForTypes { graph, types } => Ok(encode(
                &TypeGraphRenderer::new(&graph).declaration_closure_for_types(&types)?,
            )),
        }
    }

    mod erased {
        pub(super) trait Encode {
            fn encode(&self) -> serde_json::Value;
        }
        impl<T: serde::Serialize> Encode for T {
            fn encode(&self) -> serde_json::Value {
                serde_json::to_value(self).expect("generator models serialize")
            }
        }
    }

    pub(super) fn main() {
        run_with_stack(serve);
    }

    fn serve() {
        let mut input = String::new();
        let outcome = std::io::stdin()
            .read_to_string(&mut input)
            .map_err(|error| TypertGeneratorError::Workspace(error.to_string()))
            .and_then(|_| {
                serde_json::from_str::<Request>(&input).map_err(|error| {
                    TypertGeneratorError::Workspace(format!("invalid request: {error}"))
                })
            })
            .and_then(run);
        let reply = match outcome {
            Ok(value) => json!({ "ok": value }),
            Err(error) => {
                json!({ "error": { "name": error.name(), "message": error.to_string() } })
            }
        };
        println!("{reply}");
        if reply.get("error").is_some() {
            std::process::exit(1);
        }
    }
}
