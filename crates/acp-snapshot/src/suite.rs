//! Snapshot fixture validation, stable refresh, and suite orchestration.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fmt::Write as _,
    path::PathBuf,
    sync::{Arc, OnceLock},
};

use futures::future::join_all;
use parking_lot::Mutex;
use regex::Regex;
use seekdeep_core::session::is_surface_eligible_type;
use serde_json::{Map, Value};

use crate::{
    AgentUnderTest, CwdPathMode, HarvestedLog, InputScript, NormalizeContext, NormalizeOptions,
    PrepareWorkspace, RunOptions, SnapshotRunMode, extract_snapshot_spill_paths,
    normalize_session_log, normalize_stdout, run_scenario, scrub_request_headers,
    scrub_system_prompts, scrub_tool_schemas, tokenize_session_fixture_cwd,
};

const SYSTEM_PROMPT_SNAPSHOT: &str = "system-prompt.expected.md";
const TOOL_SCHEMAS_SNAPSHOT: &str = "tool-schemas.expected.json";
const WINDOWS_STDOUT_SNAPSHOT: &str = "stdout.expected.windows.jsonl";
const TOOLS_TOKEN: &str = "{{tools}}";
const MISSING_CWD_SENTINEL: &str = "\0no-cwd\0";

/// How a snapshot suite treats generated artifacts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotSuiteMode {
    /// Keyless comparison against committed fixtures.
    Replay,
    /// Live-provider fixture recording.
    Record,
    /// Keyless replay that rewrites derived fixtures.
    Refresh,
}

/// Host family used by platform-dependent suite decisions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotSuitePlatform {
    /// Microsoft Windows.
    Windows,
    /// Any non-Windows host.
    Other,
}

impl SnapshotSuitePlatform {
    const fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Other
        }
    }
}

/// One snapshot scenario and its fixture ownership declarations.
#[derive(Clone, Default)]
#[allow(clippy::struct_excessive_bools)] // The Scenario wire declares independent flags.
pub struct SnapshotScenario {
    /// Scenario directory name.
    pub name: String,
    /// Deployment environment layered into the child.
    pub environment: BTreeMap<OsString, OsString>,
    /// Whether the scenario drives at least one model turn.
    pub has_model_turn: bool,
    /// Explicit durable-log comparison override.
    pub compares_log: Option<bool>,
    /// Whether record mode regenerates this scenario.
    pub recorded: bool,
    /// Whether replay uses `replay.override.json`.
    pub overridden: bool,
    /// Whether this scenario owns its header-class pin.
    pub pins_header: bool,
    /// Header pin whose system-prompt sidecar this pin reuses.
    pub system_prompt_source: Option<String>,
    /// Header pin whose tool-schema sidecar this pin reuses.
    pub tool_schemas_source: Option<String>,
    /// One-based child fixture indices with dedicated schema sidecars.
    pub pins_child_tool_schemas: Vec<usize>,
    /// One-based child fixture indices with dedicated prompt sidecars.
    pub pins_child_system_prompts: Vec<usize>,
    /// Declared number of changed request headers in this pin.
    pub expected_header_changes: usize,
    /// Header-composition class; absent means `default`.
    pub header_class: Option<String>,
    /// Optional alternate live Cordis config.
    pub config_path: Option<PathBuf>,
    /// Optional parent for the generated workspace.
    pub workspace_parent: Option<PathBuf>,
    /// Optional final generated-workspace setup.
    pub prepare_workspace: Option<PrepareWorkspace>,
    /// Whether Windows has a second native-separator stdout golden.
    pub pins_native_windows_stdout: bool,
    /// Whether the run requires a non-Windows host.
    pub posix_only: bool,
    /// Whether the run requires an available `PowerShell` executable.
    pub pwsh_only: bool,
}

impl std::fmt::Debug for SnapshotScenario {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SnapshotScenario")
            .field("name", &self.name)
            .field("environment", &self.environment)
            .field("has_model_turn", &self.has_model_turn)
            .field("compares_log", &self.compares_log)
            .field("recorded", &self.recorded)
            .field("overridden", &self.overridden)
            .field("pins_header", &self.pins_header)
            .field("system_prompt_source", &self.system_prompt_source)
            .field("tool_schemas_source", &self.tool_schemas_source)
            .field("pins_child_tool_schemas", &self.pins_child_tool_schemas)
            .field("pins_child_system_prompts", &self.pins_child_system_prompts)
            .field("expected_header_changes", &self.expected_header_changes)
            .field("header_class", &self.header_class)
            .field("config_path", &self.config_path)
            .field("workspace_parent", &self.workspace_parent)
            .field(
                "prepare_workspace",
                &self.prepare_workspace.as_ref().map(|_| "<hook>"),
            )
            .field(
                "pins_native_windows_stdout",
                &self.pins_native_windows_stdout,
            )
            .field("posix_only", &self.posix_only)
            .field("pwsh_only", &self.pwsh_only)
            .finish()
    }
}

/// Inputs for one complete snapshot suite.
#[derive(Clone, Debug)]
pub struct SnapshotSuiteOptions {
    /// Agent composition every scenario boots.
    pub agent: AgentUnderTest,
    /// Directory containing one subdirectory per scenario.
    pub snapshots_dir: PathBuf,
    /// Registered scenarios.
    pub scenarios: Vec<SnapshotScenario>,
    /// Replay, record, or refresh behavior.
    pub mode: SnapshotSuiteMode,
    /// Result of the caller-owned `PowerShell` availability probe.
    pub has_pwsh: Option<bool>,
}

/// One stdout golden selected for a platform run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StdoutExpectedVariant {
    /// Scenario-relative file name.
    pub file: &'static str,
    /// Workspace separator policy for normalization.
    pub cwd_path_mode: CwdPathMode,
}

/// One generated claim on a shared snapshot file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SharedSnapshotClaim {
    /// Scenario that first generated the bytes.
    pub scenario: String,
    /// Complete generated file content.
    pub content: String,
}

/// One committed sidecar and its complete content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamedSnapshotContent {
    /// Diagnostic path of the committed file.
    pub path: String,
    /// Complete committed bytes decoded as UTF-8.
    pub content: String,
}

/// Parsed structured tool-schema sidecar.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolSchemasSnapshot {
    /// Initial request header's complete schemas.
    pub initial: Vec<Value>,
    /// Complete schema arrays from changed headers.
    pub changes: Vec<Vec<Value>>,
}

/// One literal refresh replacement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FixtureReplacement {
    /// Fresh replay-run value.
    pub from: String,
    /// Existing fixture value to retain.
    pub to: String,
}

/// Validated snapshot suite definition.
#[derive(Clone, Debug)]
pub struct AcpSnapshotSuite {
    options: SnapshotSuiteOptions,
    pinning_by_class: BTreeMap<String, usize>,
    prompt_source_by_class: BTreeMap<String, usize>,
    schema_source_by_class: BTreeMap<String, usize>,
}

/// Outcome metadata for one registered scenario.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotScenarioReport {
    /// Scenario name.
    pub name: String,
    /// Whether mode or host requirements skipped its subprocess run.
    pub skipped: bool,
}

/// Successful complete-suite execution report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotSuiteReport {
    /// Outcomes in registration order.
    pub scenarios: Vec<SnapshotScenarioReport>,
}

/// Validates a scenario table and resolves every header and sidecar owner.
///
/// The returned owner exposes fixture validation and execution without coupling
/// the crate to a particular test framework.
///
/// # Errors
///
/// Returns duplicate-scenario, missing/duplicate-pin, or invalid shared-source diagnostics.
pub fn define_acp_snapshot_suite(
    options: SnapshotSuiteOptions,
) -> anyhow::Result<AcpSnapshotSuite> {
    let mut scenarios_by_name = BTreeMap::new();
    for (index, scenario) in options.scenarios.iter().enumerate() {
        if scenarios_by_name
            .insert(scenario.name.clone(), index)
            .is_some()
        {
            anyhow::bail!("acp-snapshot: duplicate scenario name {:?}", scenario.name);
        }
        for (value, field) in [
            (&scenario.system_prompt_source, "systemPromptSource"),
            (&scenario.tool_schemas_source, "toolSchemasSource"),
        ] {
            if value.is_some() && !scenario.pins_header {
                anyhow::bail!(
                    "acp-snapshot: {}.{field} is only valid on a header-pinning scenario",
                    scenario.name
                );
            }
        }
    }

    let mut pinning_by_class = BTreeMap::new();
    for (index, scenario) in options.scenarios.iter().enumerate() {
        if !scenario.pins_header {
            continue;
        }
        let class = class_of(scenario).to_owned();
        if let Some(existing) = pinning_by_class.insert(class.clone(), index) {
            anyhow::bail!(
                "acp-snapshot: header class {class:?} pinned by both {} and {}",
                options.scenarios[existing].name,
                scenario.name
            );
        }
    }
    for scenario in &options.scenarios {
        if !pinning_by_class.contains_key(class_of(scenario)) {
            anyhow::bail!(
                "acp-snapshot: no scenario pins the request-header content of class {:?} (needed by {})",
                class_of(scenario),
                scenario.name
            );
        }
    }

    let mut prompt_source_by_class = BTreeMap::new();
    let mut schema_source_by_class = BTreeMap::new();
    for (class, pin_index) in &pinning_by_class {
        let pin = &options.scenarios[*pin_index];
        prompt_source_by_class.insert(
            class.clone(),
            resolve_sidecar_source(
                &options.scenarios,
                &scenarios_by_name,
                pin,
                pin.system_prompt_source.as_deref(),
                SidecarKind::SystemPrompt,
            )?,
        );
        schema_source_by_class.insert(
            class.clone(),
            resolve_sidecar_source(
                &options.scenarios,
                &scenarios_by_name,
                pin,
                pin.tool_schemas_source.as_deref(),
                SidecarKind::ToolSchemas,
            )?,
        );
    }

    Ok(AcpSnapshotSuite {
        options,
        pinning_by_class,
        prompt_source_by_class,
        schema_source_by_class,
    })
}

impl AcpSnapshotSuite {
    /// Returns the validated options backing this suite.
    #[must_use]
    pub const fn options(&self) -> &SnapshotSuiteOptions {
        &self.options
    }

    /// Runs all eligible scenarios and the complete fixture guard inventory.
    ///
    /// Replay scenarios are polled concurrently. Record and refresh runs remain serial because
    /// they own write-back order. Every started scenario settles before errors
    /// are reported, so one failure cannot cancel another scenario's cleanup.
    ///
    /// # Errors
    ///
    /// Returns every scenario failure plus any fixture-guard failure in one diagnostic.
    pub async fn run(&self) -> anyhow::Result<SnapshotSuiteReport> {
        let prompt_claims = Arc::new(Mutex::new(BTreeMap::new()));
        let schema_claims = Arc::new(Mutex::new(BTreeMap::new()));
        let mut reports = Vec::with_capacity(self.options.scenarios.len());
        let mut failures = Vec::new();

        if self.options.mode == SnapshotSuiteMode::Replay {
            let results = join_all(self.options.scenarios.iter().map(|scenario| {
                self.run_registered_scenario(scenario, prompt_claims.clone(), schema_claims.clone())
            }))
            .await;
            for (scenario, result) in self.options.scenarios.iter().zip(results) {
                match result {
                    Ok(report) => reports.push(report),
                    Err(error) => {
                        reports.push(SnapshotScenarioReport {
                            name: scenario.name.clone(),
                            skipped: false,
                        });
                        failures.push(format!("{}: {error:#}", scenario.name));
                    }
                }
            }
        } else {
            for scenario in &self.options.scenarios {
                match self
                    .run_registered_scenario(scenario, prompt_claims.clone(), schema_claims.clone())
                    .await
                {
                    Ok(report) => reports.push(report),
                    Err(error) => {
                        reports.push(SnapshotScenarioReport {
                            name: scenario.name.clone(),
                            skipped: false,
                        });
                        failures.push(format!("{}: {error:#}", scenario.name));
                    }
                }
            }
        }
        if let Err(error) = self.validate_fixtures() {
            failures.push(format!("snapshot fixtures: {error:#}"));
        }
        if !failures.is_empty() {
            anyhow::bail!("ACP snapshot suite failed:\n- {}", failures.join("\n- "));
        }
        Ok(SnapshotSuiteReport { scenarios: reports })
    }

    #[allow(clippy::too_many_lines)]
    async fn run_registered_scenario(
        &self,
        scenario: &SnapshotScenario,
        prompt_claims: Arc<Mutex<BTreeMap<String, SharedSnapshotClaim>>>,
        schema_claims: Arc<Mutex<BTreeMap<String, SharedSnapshotClaim>>>,
    ) -> anyhow::Result<SnapshotScenarioReport> {
        let recording = self.options.mode == SnapshotSuiteMode::Record;
        if scenario_skipped(
            scenario,
            recording,
            SnapshotSuitePlatform::current(),
            self.options.has_pwsh,
        ) {
            return Ok(SnapshotScenarioReport {
                name: scenario.name.clone(),
                skipped: true,
            });
        }

        let directory = self.options.snapshots_dir.join(&scenario.name);
        let input: InputScript =
            serde_json::from_slice(&tokio::fs::read(directory.join("input.json")).await?)?;
        let override_file = directory.join("replay.override.json");
        let workspace_dir = directory.join("workspace");
        let mut fixture_files = if recording {
            Vec::new()
        } else {
            session_fixtures(&directory)?
        };
        let child_files = if recording {
            Vec::new()
        } else {
            fixture_files
                .iter()
                .skip(1)
                .map(|file| directory.join(file))
                .collect()
        };
        let compares_log = scenario.compares_log.unwrap_or(scenario.has_model_turn);
        let result = run_scenario(
            &input,
            RunOptions {
                agent: self.options.agent.clone(),
                mode: if recording {
                    SnapshotRunMode::Record
                } else {
                    SnapshotRunMode::Replay
                },
                environment: scenario.environment.clone(),
                fixture_file: directory.join("session.jsonl"),
                override_file: override_file.is_file().then_some(override_file),
                child_files,
                workspace_dir: workspace_dir.is_dir().then_some(workspace_dir),
                prepare_workspace: scenario.prepare_workspace.clone(),
                workspace_parent: scenario.workspace_parent.clone(),
                config_path: scenario.config_path.clone(),
                artifact_mode: None,
            },
        )
        .await?;

        for log in &result.session_logs {
            let unknown = unknown_tool_call_ids(&log.content)?;
            if !unknown.is_empty() {
                anyhow::bail!(
                    "session {}: snapshot scenarios must not accept UNKNOWN_TOOL: {unknown:?}",
                    log.id
                );
            }
        }
        let context = NormalizeContext {
            session_ids: result
                .session_id
                .iter()
                .map(|id| id.as_str().to_owned())
                .chain(result.session_logs.iter().map(|log| log.id.clone()))
                .collect(),
            cwd: result.cwd.to_string_lossy().into_owned(),
            cwd_aliases: result
                .cwd_aliases
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
        };

        let writes_session_fixtures = (recording && scenario.recorded && scenario.has_model_turn)
            || (self.options.mode == SnapshotSuiteMode::Refresh && compares_log);
        if writes_session_fixtures {
            if result.session_logs.is_empty() {
                anyhow::bail!("{:?} produced no session log to harvest", self.options.mode);
            }
            if self.options.mode == SnapshotSuiteMode::Refresh
                && result.session_logs.len() != fixture_files.len()
            {
                anyhow::bail!(
                    "expected {} session logs (parent + children), got {}",
                    fixture_files.len(),
                    result.session_logs.len()
                );
            }
            let output_fixture_files = std::iter::once("session.jsonl".to_owned())
                .chain((1..result.session_logs.len()).map(|index| format!("session.{index}.jsonl")))
                .collect::<Vec<_>>();
            let mut existing_fixtures = Vec::with_capacity(output_fixture_files.len());
            for file in &output_fixture_files {
                existing_fixtures.push(read_if_exists(&directory.join(file)).await?);
            }
            let refresh_replacements = if self.options.mode == SnapshotSuiteMode::Refresh {
                refresh_fixture_replacements(&result.session_logs, &existing_fixtures)?
            } else {
                Vec::new()
            };
            let mut fresh_fixtures = Vec::with_capacity(result.session_logs.len());
            for (index, log) in result.session_logs.iter().enumerate() {
                let stable = if self.options.mode == SnapshotSuiteMode::Refresh {
                    stabilize_refresh_log(
                        &log.content,
                        &existing_fixtures[index],
                        &refresh_replacements,
                        &context,
                    )?
                } else {
                    log.content.clone()
                };
                let portable = if scenario.workspace_parent.is_none() {
                    tokenize_session_fixture_cwd(&stable)?
                } else {
                    stable
                };
                fresh_fixtures.push(scrub_fixture(&portable, scenario.pins_header)?);
            }
            let output_fixtures =
                stabilize_fixture_message_ids(&fresh_fixtures, &existing_fixtures)?;
            for (file, content) in output_fixture_files.iter().zip(&output_fixtures) {
                tokio::fs::write(directory.join(file), content).await?;
            }
            if recording {
                let output_names = output_fixture_files.iter().collect::<BTreeSet<_>>();
                for name in regular_file_names(&directory)? {
                    if child_session_name_re().is_match(&name) && !output_names.contains(&name) {
                        tokio::fs::remove_file(directory.join(name)).await?;
                    }
                }
                fixture_files = output_fixture_files;
            }

            if scenario.pins_header {
                let primary = &result.session_logs[0];
                let prompts = normalized_system_prompts(&primary.content, &context)?;
                if prompts.is_empty() {
                    anyhow::bail!(
                        "{:?} produced no system prompt to snapshot",
                        self.options.mode
                    );
                }
                let prompt_snapshot = format_system_prompt_snapshot(&prompts[0], &prompts[1..]);
                let prompt_source = self.prompt_source(scenario);
                let prompt_path = self
                    .options
                    .snapshots_dir
                    .join(&prompt_source.name)
                    .join(SYSTEM_PROMPT_SNAPSHOT);
                claim_shared_snapshot(
                    &mut prompt_claims.lock(),
                    &prompt_path.to_string_lossy(),
                    &scenario.name,
                    &prompt_snapshot,
                )?;
                tokio::fs::write(&prompt_path, &prompt_snapshot).await?;

                let schema_sets = normalized_tool_schemas(&primary.content, &context)?;
                if schema_sets.is_empty() {
                    anyhow::bail!(
                        "{:?} produced no tool schemas to snapshot",
                        self.options.mode
                    );
                }
                if schema_sets.len() != prompts.len() {
                    anyhow::bail!(
                        "{:?} produced a tool-schema sequence that differs from its prompt sequence",
                        self.options.mode
                    );
                }
                let schema_snapshot =
                    format_tool_schemas_snapshot(&schema_sets[0], &schema_sets[1..])?;
                let schema_source = self.schema_source(scenario);
                let schema_path = self
                    .options
                    .snapshots_dir
                    .join(&schema_source.name)
                    .join(TOOL_SCHEMAS_SNAPSHOT);
                claim_shared_snapshot(
                    &mut schema_claims.lock(),
                    &schema_path.to_string_lossy(),
                    &scenario.name,
                    &schema_snapshot,
                )?;
                tokio::fs::write(&schema_path, &schema_snapshot).await?;
            }
            for index in &scenario.pins_child_tool_schemas {
                let Some(log) = result.session_logs.get(*index) else {
                    anyhow::bail!(
                        "{:?}: no child session log at index {index} to snapshot schemas from",
                        self.options.mode
                    );
                };
                let schemas = normalized_tool_schemas(&log.content, &context)?;
                if schemas.is_empty() {
                    anyhow::bail!(
                        "{:?}: child {index} produced no tool schemas to snapshot",
                        self.options.mode
                    );
                }
                tokio::fs::write(
                    directory.join(child_tool_schemas_snapshot(*index)),
                    format_tool_schemas_snapshot(&schemas[0], &schemas[1..])?,
                )
                .await?;
            }
            for index in &scenario.pins_child_system_prompts {
                let Some(log) = result.session_logs.get(*index) else {
                    anyhow::bail!(
                        "{:?}: no child session log at index {index} to snapshot a prompt from",
                        self.options.mode
                    );
                };
                let prompts = normalized_system_prompts(&log.content, &context)?;
                if prompts.is_empty() {
                    anyhow::bail!(
                        "{:?}: child {index} produced no system prompt to snapshot",
                        self.options.mode
                    );
                }
                tokio::fs::write(
                    directory.join(child_system_prompt_snapshot(*index)),
                    format_system_prompt_snapshot(&prompts[0], &[]),
                )
                .await?;
            }
        }

        for expected in stdout_expected_variants(scenario, SnapshotSuitePlatform::current()) {
            let stdout = normalize_stdout(
                &result.raw_stdout,
                &context,
                NormalizeOptions {
                    cwd_path_mode: expected.cwd_path_mode,
                },
            )?;
            let path = directory.join(expected.file);
            if self.options.mode != SnapshotSuiteMode::Replay {
                tokio::fs::write(&path, &stdout).await?;
            }
            let committed = tokio::fs::read_to_string(&path).await?;
            if stdout != committed {
                anyhow::bail!("{} mismatch", expected.file);
            }
        }

        if compares_log {
            if result.session_logs.len() != fixture_files.len() {
                anyhow::bail!(
                    "this scenario must persist one log per session fixture: got {}, expected {}",
                    result.session_logs.len(),
                    fixture_files.len()
                );
            }
            for (index, file) in fixture_files.iter().enumerate() {
                let harvested =
                    scrub_fixture(&result.session_logs[index].content, scenario.pins_header)?;
                let fixture = scrub_fixture(
                    &tokio::fs::read_to_string(directory.join(file)).await?,
                    scenario.pins_header,
                )?;
                let harvested =
                    normalize_session_log(&harvested, &context, NormalizeOptions::default())?;
                let expected = normalize_session_log(
                    &fixture,
                    &fixture_context(&fixture)?,
                    NormalizeOptions::default(),
                )?;
                if harvested != expected {
                    anyhow::bail!("{file} mismatch");
                }
            }
        }

        self.validate_live_headers(scenario, &result.session_logs, &context)
            .await?;
        Ok(SnapshotScenarioReport {
            name: scenario.name.clone(),
            skipped: false,
        })
    }

    #[allow(clippy::too_many_lines)]
    async fn validate_live_headers(
        &self,
        scenario: &SnapshotScenario,
        logs: &[HarvestedLog],
        context: &NormalizeContext,
    ) -> anyhow::Result<()> {
        let pin = self.pinning_scenario(scenario);
        let prompt_source = self.prompt_source(scenario);
        let schema_source = self.schema_source(scenario);
        let pinning_directory = self.options.snapshots_dir.join(&pin.name);
        let pinned_fixture =
            tokio::fs::read_to_string(pinning_directory.join("session.jsonl")).await?;
        let pinned = normalized_headers(&pinned_fixture, &fixture_context(&pinned_fixture)?)?;
        let prompt_snapshot = tokio::fs::read_to_string(
            self.options
                .snapshots_dir
                .join(&prompt_source.name)
                .join(SYSTEM_PROMPT_SNAPSHOT),
        )
        .await?;
        let initial_prompt = initial_system_prompt_snapshot(&prompt_snapshot);
        if pinned.len() != 1 + pin.expected_header_changes {
            anyhow::bail!(
                "the pinning fixture ({}) has an unexpected request/header count",
                pin.name
            );
        }
        let schema_snapshot = tokio::fs::read_to_string(
            self.options
                .snapshots_dir
                .join(&schema_source.name)
                .join(TOOL_SCHEMAS_SNAPSHOT),
        )
        .await?;
        let schemas = parse_tool_schemas_snapshot(&schema_snapshot)?;
        let schema_sets = std::iter::once(&schemas.initial)
            .chain(&schemas.changes)
            .collect::<Vec<_>>();
        if schema_sets.len() != pinned.len() {
            anyhow::bail!(
                "the schema source ({}) has an unexpected tool-schema count",
                schema_source.name
            );
        }
        let pinned_headers = pinned
            .iter()
            .zip(schema_sets)
            .map(|(header, schemas)| restore_pinned_tool_schemas(header, schemas))
            .collect::<anyhow::Result<Vec<_>>>()?;

        let directory = self.options.snapshots_dir.join(&scenario.name);
        let mut child_pinned_schemas = BTreeMap::new();
        for index in &scenario.pins_child_tool_schemas {
            let sidecar =
                tokio::fs::read_to_string(directory.join(child_tool_schemas_snapshot(*index)))
                    .await?;
            let parsed = parse_tool_schemas_snapshot(&sidecar)?;
            child_pinned_schemas.insert(
                *index,
                std::iter::once(parsed.initial)
                    .chain(parsed.changes)
                    .collect::<Vec<_>>(),
            );
        }
        let mut child_pinned_prompts = BTreeMap::new();
        for index in &scenario.pins_child_system_prompts {
            child_pinned_prompts.insert(
                *index,
                tokio::fs::read_to_string(directory.join(child_system_prompt_snapshot(*index)))
                    .await?,
            );
        }
        for (log_index, log) in logs.iter().enumerate() {
            let expected_changes = if scenario.pins_header && log_index == 0 {
                scenario.expected_header_changes
            } else {
                0
            };
            if header_change_count(&log.content)? != expected_changes {
                anyhow::bail!("session {}: changed request/header count", log.id);
            }
            let headers = normalized_headers(&scrub_system_prompts(&log.content)?, context)?;
            let prompts = normalized_system_prompts(&log.content, context)?;
            let schema_sets = normalized_tool_schemas(&log.content, context)?;
            if prompts.len() != headers.len() {
                anyhow::bail!(
                    "session {}: every request/header must carry a string system prompt",
                    log.id
                );
            }
            if schema_sets.len() != headers.len() {
                anyhow::bail!(
                    "session {}: every request/header must carry an array-valued tools field",
                    log.id
                );
            }
            let child_schemas = child_pinned_schemas.get(&log_index);
            if child_schemas.is_some_and(|schemas| schemas.len() != schema_sets.len()) {
                anyhow::bail!(
                    "session {}: child tool-schema sidecar has an unexpected count",
                    log.id
                );
            }
            for (header_index, header) in headers.iter().enumerate() {
                let class_pin = if expected_changes > 0 {
                    pinned_headers.get(header_index).ok_or_else(|| {
                        anyhow::anyhow!(
                            "session {}: request/header #{} has no pinned changed header",
                            log.id,
                            header_index + 1
                        )
                    })?
                } else {
                    &pinned_headers[0]
                };
                let expected = if let Some(child_schemas) = child_schemas {
                    let mut expected = class_pin
                        .as_object()
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("pinned header must be an object"))?;
                    expected.insert(
                        "tools".to_owned(),
                        Value::Array(child_schemas[header_index].clone()),
                    );
                    Value::Object(expected)
                } else {
                    class_pin.clone()
                };
                if *header != expected {
                    anyhow::bail!(
                        "session {}: request/header #{} diverged from the pinned ({}) header",
                        log.id,
                        header_index + 1,
                        pin.name
                    );
                }
                if expected_changes == 0 {
                    let expected_prompt = child_pinned_prompts
                        .get(&log_index)
                        .map_or(initial_prompt, String::as_str);
                    if format_system_prompt_snapshot(&prompts[header_index], &[]) != expected_prompt
                    {
                        anyhow::bail!(
                            "session {}: initial system prompt #{} diverged from its sidecar",
                            log.id,
                            header_index + 1
                        );
                    }
                }
            }
            if scenario.pins_header && log_index == 0 {
                if prompts.is_empty() || schema_sets.is_empty() {
                    anyhow::bail!(
                        "session {}: a pinning log must carry a prompt and tool-schema header",
                        log.id
                    );
                }
                if format_system_prompt_snapshot(&prompts[0], &prompts[1..]) != prompt_snapshot {
                    anyhow::bail!("session {}: changed system prompts diverged", log.id);
                }
                if format_tool_schemas_snapshot(&schema_sets[0], &schema_sets[1..])?
                    != schema_snapshot
                {
                    anyhow::bail!("session {}: changed tool schemas diverged", log.id);
                }
            }
        }
        Ok(())
    }

    fn pinning_scenario(&self, scenario: &SnapshotScenario) -> &SnapshotScenario {
        &self.options.scenarios[self.pinning_by_class[class_of(scenario)]]
    }

    fn prompt_source(&self, scenario: &SnapshotScenario) -> &SnapshotScenario {
        &self.options.scenarios[self.prompt_source_by_class[class_of(scenario)]]
    }

    fn schema_source(&self, scenario: &SnapshotScenario) -> &SnapshotScenario {
        &self.options.scenarios[self.schema_source_by_class[class_of(scenario)]]
    }

    /// Applies every fixture guard to the committed snapshot tree.
    ///
    /// # Errors
    ///
    /// Returns orphan/missing fixtures, malformed inventories, invalid pins,
    /// duplicate sidecar bytes, unknown tools, or noncanonical storage.
    #[allow(clippy::too_many_lines)]
    pub fn validate_fixtures(&self) -> anyhow::Result<()> {
        let mut on_disk = std::fs::read_dir(&self.options.snapshots_dir)?
            .filter_map(Result::ok)
            .filter_map(|entry| {
                entry
                    .file_type()
                    .ok()
                    .filter(std::fs::FileType::is_dir)
                    .and_then(|_| entry.file_name().into_string().ok())
            })
            .collect::<Vec<_>>();
        on_disk.sort();
        let mut registered = self
            .options
            .scenarios
            .iter()
            .map(|scenario| scenario.name.clone())
            .collect::<Vec<_>>();
        registered.sort();
        if on_disk != registered {
            anyhow::bail!(
                "acp-snapshot: scenario directories do not match registration: disk={on_disk:?}, registered={registered:?}"
            );
        }

        let prompt_owners = self
            .prompt_source_by_class
            .values()
            .map(|index| self.options.scenarios[*index].name.as_str())
            .collect::<BTreeSet<_>>();
        let schema_owners = self
            .schema_source_by_class
            .values()
            .map(|index| self.options.scenarios[*index].name.as_str())
            .collect::<BTreeSet<_>>();

        for scenario in &self.options.scenarios {
            let directory = self.options.snapshots_dir.join(&scenario.name);
            let names = regular_file_names(&directory)?;
            require_child_sidecar_inventory(
                scenario,
                &names,
                &scenario.pins_child_tool_schemas,
                child_tool_schema_name_re(),
                "child tool-schema sidecars must match `pinsChildToolSchemas`",
            )?;
            require_child_sidecar_inventory(
                scenario,
                &names,
                &scenario.pins_child_system_prompts,
                child_system_prompt_name_re(),
                "child system-prompt sidecars must match `pinsChildSystemPrompts`",
            )?;
            require_file(&directory, "input.json", true, "input.json")?;
            require_file(
                &directory,
                "stdout.expected.jsonl",
                true,
                "stdout.expected.jsonl",
            )?;
            require_file(
                &directory,
                WINDOWS_STDOUT_SNAPSHOT,
                scenario.pins_native_windows_stdout,
                "stdout.expected.windows.jsonl presence must match `pinsNativeWindowsStdout`",
            )?;
            require_file(&directory, "session.jsonl", true, "session.jsonl")?;
            require_file(
                &directory,
                "replay.override.json",
                scenario.overridden,
                "replay.override.json presence must match `overridden`",
            )?;
            require_file(
                &directory,
                SYSTEM_PROMPT_SNAPSHOT,
                prompt_owners.contains(scenario.name.as_str()),
                "system-prompt.expected.md presence must match snapshot-source ownership",
            )?;
            require_file(
                &directory,
                TOOL_SCHEMAS_SNAPSHOT,
                schema_owners.contains(scenario.name.as_str()),
                "tool-schemas.expected.json presence must match snapshot-source ownership",
            )?;
            session_fixtures(&directory)?;
        }

        for (class, pin_index) in &self.pinning_by_class {
            let scenario = &self.options.scenarios[*pin_index];
            let prompt_source = &self.options.scenarios[self.prompt_source_by_class[class]];
            let schema_source = &self.options.scenarios[self.schema_source_by_class[class]];
            let directory = self.options.snapshots_dir.join(&scenario.name);
            let fixture = std::fs::read_to_string(directory.join("session.jsonl"))?;
            let headers = normalized_headers(&fixture, &fixture_context(&fixture)?)?;
            if headers.len() != 1 + scenario.expected_header_changes {
                anyhow::bail!(
                    "acp-snapshot: {} has {} request headers, expected {}",
                    scenario.name,
                    headers.len(),
                    1 + scenario.expected_header_changes
                );
            }
            let prompt_snapshot = std::fs::read_to_string(
                self.options
                    .snapshots_dir
                    .join(&prompt_source.name)
                    .join(SYSTEM_PROMPT_SNAPSHOT),
            )?;
            if prompt_snapshot.is_empty() || !prompt_snapshot.ends_with('\n') {
                anyhow::bail!(
                    "acp-snapshot: {}/{} must be non-empty and end in a newline",
                    prompt_source.name,
                    SYSTEM_PROMPT_SNAPSHOT
                );
            }
            let schema_text = std::fs::read_to_string(
                self.options
                    .snapshots_dir
                    .join(&schema_source.name)
                    .join(TOOL_SCHEMAS_SNAPSHOT),
            )?;
            let schemas = parse_tool_schemas_snapshot(&schema_text)?;
            if 1 + schemas.changes.len() != headers.len() {
                anyhow::bail!(
                    "acp-snapshot: {} tool-schema sequence must match {} header sequence",
                    schema_source.name,
                    scenario.name
                );
            }
            for (header, schemas) in headers
                .iter()
                .zip(std::iter::once(&schemas.initial).chain(&schemas.changes))
            {
                restore_pinned_tool_schemas(header, schemas)?;
            }
            if schema_text != format_tool_schemas_snapshot(&schemas.initial, &schemas.changes)? {
                anyhow::bail!(
                    "acp-snapshot: {}/{} must use canonical JSON formatting",
                    schema_source.name,
                    TOOL_SCHEMAS_SNAPSHOT
                );
            }
            if header_change_count(&fixture)? != scenario.expected_header_changes {
                anyhow::bail!(
                    "acp-snapshot: {} changed request-header count differs from its declaration",
                    scenario.name
                );
            }
        }

        let prompt_snapshots = prompt_owners
            .iter()
            .map(|owner| {
                Ok(NamedSnapshotContent {
                    path: format!("{owner}/{SYSTEM_PROMPT_SNAPSHOT}"),
                    content: std::fs::read_to_string(
                        self.options
                            .snapshots_dir
                            .join(owner)
                            .join(SYSTEM_PROMPT_SNAPSHOT),
                    )?,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let schema_snapshots = schema_owners
            .iter()
            .map(|owner| {
                Ok(NamedSnapshotContent {
                    path: format!("{owner}/{TOOL_SCHEMAS_SNAPSHOT}"),
                    content: std::fs::read_to_string(
                        self.options
                            .snapshots_dir
                            .join(owner)
                            .join(TOOL_SCHEMAS_SNAPSHOT),
                    )?,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        assert_unique_snapshot_contents("system-prompt", &prompt_snapshots)?;
        assert_unique_snapshot_contents("tool-schema", &schema_snapshots)?;

        for scenario in &self.options.scenarios {
            let directory = self.options.snapshots_dir.join(&scenario.name);
            let fixtures = session_fixtures(&directory)?;
            for index in &scenario.pins_child_tool_schemas {
                if fixtures.get(*index).is_none() {
                    anyhow::bail!(
                        "acp-snapshot: {} child schema pin {index} must name an existing session.<n>.jsonl fixture",
                        scenario.name
                    );
                }
                let file = child_tool_schemas_snapshot(*index);
                let sidecar = std::fs::read_to_string(directory.join(&file))?;
                let parsed = parse_tool_schemas_snapshot(&sidecar)?;
                if sidecar != format_tool_schemas_snapshot(&parsed.initial, &parsed.changes)? {
                    anyhow::bail!(
                        "acp-snapshot: {}/{} must use canonical JSON formatting",
                        scenario.name,
                        file
                    );
                }
                if parsed.initial.is_empty() {
                    anyhow::bail!(
                        "acp-snapshot: {}/{} must pin at least one schema",
                        scenario.name,
                        file
                    );
                }
            }
            for index in &scenario.pins_child_system_prompts {
                if fixtures.get(*index).is_none() {
                    anyhow::bail!(
                        "acp-snapshot: {} child prompt pin {index} must name an existing session.<n>.jsonl fixture",
                        scenario.name
                    );
                }
                let file = child_system_prompt_snapshot(*index);
                let sidecar = std::fs::read_to_string(directory.join(&file))?;
                let prompt_source =
                    &self.options.scenarios[self.prompt_source_by_class[class_of(scenario)]];
                let class_pin = std::fs::read_to_string(
                    self.options
                        .snapshots_dir
                        .join(&prompt_source.name)
                        .join(SYSTEM_PROMPT_SNAPSHOT),
                )?;
                assert_child_system_prompt_snapshot(
                    &sidecar,
                    initial_system_prompt_snapshot(&class_pin),
                    &format!("{}/{}", scenario.name, file),
                )?;
            }
            for file in fixtures {
                let fixture = std::fs::read_to_string(directory.join(&file))?;
                let unknown = unknown_tool_call_ids(&fixture)?;
                if !unknown.is_empty() {
                    anyhow::bail!(
                        "acp-snapshot: {}/{} contains UNKNOWN_TOOL calls {unknown:?}",
                        scenario.name,
                        file
                    );
                }
                if fixture.contains("/private{{cwd}}") {
                    anyhow::bail!(
                        "acp-snapshot: {}/{} carries a non-canonical macOS cwd token",
                        scenario.name,
                        file
                    );
                }
                if crate::scrub_system_prompts(&fixture)? != fixture {
                    anyhow::bail!(
                        "acp-snapshot: {}/{} carries an unscrubbed system prompt",
                        scenario.name,
                        file
                    );
                }
                if crate::scrub_tool_schemas(&fixture)? != fixture {
                    anyhow::bail!(
                        "acp-snapshot: {}/{} carries unscrubbed tool schemas",
                        scenario.name,
                        file
                    );
                }
                if !scenario.pins_header && crate::scrub_request_headers(&fixture)? != fixture {
                    anyhow::bail!(
                        "acp-snapshot: {}/{} carries unscrubbed header content",
                        scenario.name,
                        file
                    );
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum SidecarKind {
    SystemPrompt,
    ToolSchemas,
}

impl SidecarKind {
    fn field(self, scenario: &SnapshotScenario) -> Option<&str> {
        match self {
            Self::SystemPrompt => scenario.system_prompt_source.as_deref(),
            Self::ToolSchemas => scenario.tool_schemas_source.as_deref(),
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::SystemPrompt => "system-prompt snapshot",
            Self::ToolSchemas => "tool-schema snapshot",
        }
    }
}

fn resolve_sidecar_source(
    scenarios: &[SnapshotScenario],
    scenarios_by_name: &BTreeMap<String, usize>,
    pin: &SnapshotScenario,
    requested: Option<&str>,
    kind: SidecarKind,
) -> anyhow::Result<usize> {
    let source_name = requested.unwrap_or(&pin.name);
    let Some(source_index) = scenarios_by_name.get(source_name).copied() else {
        anyhow::bail!(
            "acp-snapshot: {} names unknown {} source {source_name:?}",
            pin.name,
            kind.label()
        );
    };
    let source = &scenarios[source_index];
    if !source.pins_header {
        anyhow::bail!(
            "acp-snapshot: {} names non-pinning {} source {source_name:?}",
            pin.name,
            kind.label()
        );
    }
    if kind
        .field(source)
        .is_some_and(|redirect| redirect != source.name)
    {
        anyhow::bail!(
            "acp-snapshot: {} names {} source {source_name:?}, which does not own its sidecar",
            pin.name,
            kind.label()
        );
    }
    if source.expected_header_changes != pin.expected_header_changes {
        anyhow::bail!(
            "acp-snapshot: {} and {source_name} declare different header-change counts for shared {}",
            pin.name,
            kind.label()
        );
    }
    Ok(source_index)
}

fn class_of(scenario: &SnapshotScenario) -> &str {
    scenario.header_class.as_deref().unwrap_or("default")
}

fn regular_file_names(directory: &std::path::Path) -> anyhow::Result<Vec<String>> {
    Ok(std::fs::read_dir(directory)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            entry
                .file_type()
                .ok()
                .filter(std::fs::FileType::is_file)
                .and_then(|_| entry.file_name().into_string().ok())
        })
        .collect())
}

async fn read_if_exists(path: &std::path::Path) -> anyhow::Result<String> {
    match tokio::fs::read_to_string(path).await {
        Ok(content) => Ok(content),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error.into()),
    }
}

fn scrub_fixture(content: &str, pins_header: bool) -> anyhow::Result<String> {
    if pins_header {
        Ok(scrub_tool_schemas(&scrub_system_prompts(content)?)?)
    } else {
        Ok(scrub_request_headers(content)?)
    }
}

fn session_fixtures(directory: &std::path::Path) -> anyhow::Result<Vec<String>> {
    session_fixture_names(&regular_file_names(directory)?)
}

fn require_file(
    directory: &std::path::Path,
    file: &str,
    expected: bool,
    label: &str,
) -> anyhow::Result<()> {
    let actual = directory.join(file).is_file();
    if actual != expected {
        anyhow::bail!(
            "acp-snapshot: {}/{file}: {label}; expected presence {expected}, found {actual}",
            directory.display()
        );
    }
    Ok(())
}

fn require_child_sidecar_inventory(
    scenario: &SnapshotScenario,
    names: &[String],
    declared: &[usize],
    expression: &Regex,
    label: &str,
) -> anyhow::Result<()> {
    let found = names
        .iter()
        .filter_map(|name| expression.captures(name))
        .filter_map(|captures| captures[1].parse::<usize>().ok())
        .collect::<BTreeSet<_>>();
    let expected = declared.iter().copied().collect::<BTreeSet<_>>();
    if found != expected {
        anyhow::bail!(
            "acp-snapshot: {}: {label}; expected {expected:?}, found {found:?}",
            scenario.name
        );
    }
    Ok(())
}

/// Whether a scenario's run is skipped for a mode and host.
#[must_use]
pub fn scenario_skipped(
    scenario: &SnapshotScenario,
    recording: bool,
    platform: SnapshotSuitePlatform,
    has_pwsh: Option<bool>,
) -> bool {
    (recording && !scenario.recorded)
        || (scenario.posix_only && platform == SnapshotSuitePlatform::Windows)
        || (scenario.pwsh_only && has_pwsh != Some(true))
}

/// Shared stdout golden followed by an optional Windows-native golden.
#[must_use]
pub fn stdout_expected_variants(
    scenario: &SnapshotScenario,
    platform: SnapshotSuitePlatform,
) -> Vec<StdoutExpectedVariant> {
    let canonical = StdoutExpectedVariant {
        file: "stdout.expected.jsonl",
        cwd_path_mode: CwdPathMode::Canonical,
    };
    if platform != SnapshotSuitePlatform::Windows || !scenario.pins_native_windows_stdout {
        return vec![canonical];
    }
    vec![
        canonical,
        StdoutExpectedVariant {
            file: WINDOWS_STDOUT_SNAPSHOT,
            cwd_path_mode: CwdPathMode::Native,
        },
    ]
}

/// Records a scenario's generated bytes for one shared snapshot path.
///
/// # Errors
///
/// Returns a divergence diagnostic when a later claim differs.
pub fn claim_shared_snapshot(
    claims: &mut BTreeMap<String, SharedSnapshotClaim>,
    source: &str,
    scenario: &str,
    content: &str,
) -> anyhow::Result<()> {
    if let Some(previous) = claims.get(source) {
        if previous.content != content {
            anyhow::bail!(
                "acp-snapshot: shared snapshot {source} diverged between {} and {scenario}",
                previous.scenario
            );
        }
        return Ok(());
    }
    claims.insert(
        source.to_owned(),
        SharedSnapshotClaim {
            scenario: scenario.to_owned(),
            content: content.to_owned(),
        },
    );
    Ok(())
}

/// Rejects byte-identical committed snapshots stored under different paths.
///
/// # Errors
///
/// Returns the duplicate-content diagnostic.
pub fn assert_unique_snapshot_contents(
    kind: &str,
    snapshots: &[NamedSnapshotContent],
) -> anyhow::Result<()> {
    let mut first_path_by_content = BTreeMap::<&str, &str>::new();
    for snapshot in snapshots {
        if let Some(first) = first_path_by_content.get(snapshot.content.as_str()) {
            anyhow::bail!(
                "acp-snapshot: identical {kind} snapshots appear in {first} and {}; reuse one source",
                snapshot.path
            );
        }
        first_path_by_content.insert(&snapshot.content, &snapshot.path);
    }
    Ok(())
}

/// Validates and orders primary plus contiguous child Session fixture names.
///
/// # Errors
///
/// Returns missing-primary, malformed-child, or gapped-child diagnostics.
pub fn session_fixture_names(names: &[String]) -> anyhow::Result<Vec<String>> {
    if !names.iter().any(|name| name == "session.jsonl") {
        anyhow::bail!("missing session.jsonl");
    }
    let mut children = Vec::<(usize, String)>::new();
    for name in names {
        if name == "session.jsonl"
            || !name.starts_with("session.")
            || PathBuf::from(name)
                .extension()
                .and_then(|value| value.to_str())
                != Some("jsonl")
        {
            continue;
        }
        let Some(captures) = child_session_name_re().captures(name) else {
            anyhow::bail!("invalid child session fixture name: {name}");
        };
        let index = captures[1].parse::<usize>()?;
        children.push((index, name.clone()));
    }
    children.sort_by_key(|(index, _)| *index);
    for (offset, (index, name)) in children.iter().enumerate() {
        let expected = offset + 1;
        if *index != expected {
            anyhow::bail!(
                "child session fixtures must be contiguous: expected session.{expected}.jsonl, found {name}"
            );
        }
    }
    Ok(std::iter::once("session.jsonl".to_owned())
        .chain(children.into_iter().map(|(_, name)| name))
        .collect())
}

/// Derives volatile normalization values from a committed Session header.
///
/// # Errors
///
/// Returns malformed header JSON.
pub fn fixture_context(fixture: &str) -> anyhow::Result<NormalizeContext> {
    let first = fixture
        .split('\n')
        .find(|line| !line.trim().is_empty())
        .unwrap_or("{}");
    let header: Value = serde_json::from_str(first)?;
    Ok(NormalizeContext {
        session_ids: header
            .get("id")
            .and_then(Value::as_str)
            .map_or_else(Vec::new, |id| vec![id.to_owned()]),
        cwd: header
            .get("cwd")
            .and_then(Value::as_str)
            .unwrap_or(MISSING_CWD_SENTINEL)
            .to_owned(),
        cwd_aliases: Vec::new(),
    })
}

/// Extracts normalized request-header payloads in log order.
///
/// # Errors
///
/// Returns malformed JSONL or normalization failures.
pub fn normalized_headers(raw_log: &str, context: &NormalizeContext) -> anyhow::Result<Vec<Value>> {
    Ok(parse_jsonl_values(&normalize_session_log(
        raw_log,
        context,
        NormalizeOptions::default(),
    )?)?
    .into_iter()
    .filter(|record| record.get("type").and_then(Value::as_str) == Some("request/header"))
    .map(|record| {
        record
            .pointer("/data/header")
            .cloned()
            .unwrap_or(Value::Null)
    })
    .collect())
}

/// Extracts normalized string system prompts in header order.
///
/// # Errors
///
/// Returns malformed JSONL or normalization failures.
pub fn normalized_system_prompts(
    raw_log: &str,
    context: &NormalizeContext,
) -> anyhow::Result<Vec<String>> {
    Ok(normalized_headers(raw_log, context)?
        .into_iter()
        .filter_map(|header| {
            header
                .get("system")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect())
}

/// Extracts normalized array-valued tool schemas in header order.
///
/// # Errors
///
/// Returns malformed JSONL or normalization failures.
pub fn normalized_tool_schemas(
    raw_log: &str,
    context: &NormalizeContext,
) -> anyhow::Result<Vec<Vec<Value>>> {
    Ok(normalized_headers(raw_log, context)?
        .into_iter()
        .filter_map(|header| header.get("tools").and_then(Value::as_array).cloned())
        .collect())
}

/// Formats a canonical structured tool-schema sidecar.
///
/// # Errors
///
/// Returns serialization failures.
pub fn format_tool_schemas_snapshot(
    initial: &[Value],
    changes: &[Vec<Value>],
) -> anyhow::Result<String> {
    let mut object = Map::new();
    object.insert("initial".to_owned(), Value::Array(initial.to_vec()));
    object.insert(
        "changes".to_owned(),
        Value::Array(changes.iter().cloned().map(Value::Array).collect()),
    );
    Ok(format!(
        "{}\n",
        serde_json::to_string_pretty(&Value::Object(object))?
    ))
}

/// Parses and validates a structured tool-schema sidecar.
///
/// # Errors
///
/// Returns malformed JSON or stable shape diagnostics.
pub fn parse_tool_schemas_snapshot(snapshot: &str) -> anyhow::Result<ToolSchemasSnapshot> {
    let parsed: Value = serde_json::from_str(snapshot)?;
    let Some(object) = parsed.as_object() else {
        anyhow::bail!("acp-snapshot: tool-schema snapshot must be an object");
    };
    let (Some(initial), Some(changes)) = (
        object.get("initial").and_then(Value::as_array),
        object.get("changes").and_then(Value::as_array),
    ) else {
        anyhow::bail!(
            "acp-snapshot: tool-schema snapshot must carry array-valued initial and changes fields"
        );
    };
    if !changes.iter().all(Value::is_array) {
        anyhow::bail!(
            "acp-snapshot: tool-schema snapshot must carry array-valued initial and changes fields"
        );
    }
    Ok(ToolSchemasSnapshot {
        initial: initial.clone(),
        changes: changes
            .iter()
            .filter_map(Value::as_array)
            .cloned()
            .collect(),
    })
}

/// Restores one sidecar schema set into a tokenized pinned header.
///
/// # Errors
///
/// Returns invalid-header or missing-token diagnostics.
pub fn restore_pinned_tool_schemas(header: &Value, schemas: &[Value]) -> anyhow::Result<Value> {
    let Some(mut object) = header.as_object().cloned() else {
        anyhow::bail!("acp-snapshot: pinned request header must be an object");
    };
    if object.get("tools").and_then(Value::as_str) != Some(TOOLS_TOKEN) {
        anyhow::bail!("acp-snapshot: pinned request header tools must equal {TOOLS_TOKEN}");
    }
    object.insert("tools".to_owned(), Value::Array(schemas.to_vec()));
    Ok(Value::Object(object))
}

/// Formats a normalized system prompt as a repository-friendly sidecar.
#[must_use]
pub fn format_system_prompt_snapshot(prompt: &str, changes: &[String]) -> String {
    let mut snapshot = prompt.to_owned();
    if !snapshot.ends_with('\n') {
        snapshot.push('\n');
    }
    for (index, change) in changes.iter().enumerate() {
        write!(
            snapshot,
            "\n<!-- request/header change {} -->\n\n",
            index + 1
        )
        .expect("writing to a String is infallible");
        snapshot.push_str(change);
        if !change.ends_with('\n') {
            snapshot.push('\n');
        }
    }
    snapshot
}

/// Validates a distinct canonical child system-prompt sidecar.
///
/// # Errors
///
/// Returns empty, missing-newline, or duplicate-class-pin diagnostics.
pub fn assert_child_system_prompt_snapshot(
    sidecar: &str,
    class_pin: &str,
    label: &str,
) -> anyhow::Result<()> {
    if sidecar.trim().is_empty() {
        anyhow::bail!("{label} must pin a non-empty prompt");
    }
    if !sidecar.ends_with('\n') {
        anyhow::bail!("{label} must end in a newline");
    }
    if sidecar == class_pin {
        anyhow::bail!("{label} must differ from its class pin");
    }
    Ok(())
}

/// Counts changed request-header snapshots in Session JSONL.
///
/// # Errors
///
/// Returns malformed JSONL.
pub fn header_change_count(raw_log: &str) -> anyhow::Result<usize> {
    Ok(parse_jsonl_values(raw_log)?
        .iter()
        .filter(|record| {
            record.get("type").and_then(Value::as_str) == Some("request/header")
                && record.pointer("/data/reason").and_then(Value::as_str) == Some("change")
        })
        .count())
}

/// Finds tool calls whose structured result reports `UNKNOWN_TOOL`.
///
/// # Errors
///
/// Returns malformed JSONL or non-object records.
pub fn unknown_tool_call_ids(raw_log: &str) -> anyhow::Result<Vec<String>> {
    Ok(parse_jsonl_records(raw_log)?
        .iter()
        .filter_map(|record| {
            if record.get("type").and_then(Value::as_str) != Some("tool/result")
                || record
                    .get("data")
                    .and_then(Value::as_object)
                    .and_then(|data| data.get("error"))
                    .and_then(Value::as_object)
                    .and_then(|error| error.get("code"))
                    .and_then(Value::as_str)
                    != Some("UNKNOWN_TOOL")
            {
                return None;
            }
            Some(
                record
                    .get("data")
                    .and_then(Value::as_object)
                    .and_then(|data| data.get("message"))
                    .and_then(Value::as_object)
                    .and_then(|message| message.get("source"))
                    .and_then(Value::as_object)
                    .and_then(|source| source.get("callId"))
                    .and_then(Value::as_str)
                    .unwrap_or("<missing callId>")
                    .to_owned(),
            )
        })
        .collect())
}

/// Carries committed UUIDs into unchanged, unambiguous durable messages.
///
/// # Errors
///
/// Returns malformed fixture JSONL.
pub fn stabilize_fixture_message_ids(
    logs: &[String],
    fixtures: &[String],
) -> anyhow::Result<Vec<String>> {
    let replacements = fixture_message_id_replacements(logs, fixtures)?;
    logs.iter()
        .map(|log| apply_fixture_message_ids(log, &replacements))
        .collect()
}

/// Builds refresh replacements for Session ids, cwd values, and spill paths.
///
/// # Errors
///
/// Returns malformed fresh or existing JSONL.
pub fn refresh_fixture_replacements(
    logs: &[HarvestedLog],
    fixtures: &[String],
) -> anyhow::Result<Vec<FixtureReplacement>> {
    let mut replacements = Vec::new();
    for (index, log) in logs.iter().enumerate() {
        let fresh = parse_jsonl_records(&log.content)?.into_iter().next();
        let existing = parse_jsonl_records(fixtures.get(index).map_or("", String::as_str))?
            .into_iter()
            .next();
        for field in ["id", "cwd"] {
            let from = fresh
                .as_ref()
                .and_then(|record| record.get(field))
                .and_then(Value::as_str);
            let to = existing
                .as_ref()
                .and_then(|record| record.get(field))
                .and_then(Value::as_str);
            if let (Some(from), Some(to)) = (from, to)
                && !from.is_empty()
                && from != to
            {
                replacements.push(FixtureReplacement {
                    from: from.to_owned(),
                    to: to.to_owned(),
                });
            }
        }
        let fresh_spills = extract_snapshot_spill_paths(&log.content);
        let existing_spills =
            extract_snapshot_spill_paths(fixtures.get(index).map_or("", String::as_str));
        for (name, existing_path) in existing_spills {
            if let Some(fresh_path) = fresh_spills.get(&name)
                && fresh_path != &existing_path
            {
                replacements.push(FixtureReplacement {
                    from: fresh_path.clone(),
                    to: existing_path,
                });
            }
        }
    }
    Ok(replacements)
}

/// Stabilizes a replay-produced log against an existing committed fixture.
///
/// # Errors
///
/// Returns malformed JSONL, normalization failures, or an invalid inserted-title boundary.
pub fn stabilize_refresh_log(
    fresh: &str,
    existing: &str,
    replacements: &[FixtureReplacement],
    fresh_context: &NormalizeContext,
) -> anyhow::Result<String> {
    let fresh_records = parse_jsonl_records(fresh)?;
    let stable = apply_fixture_replacements(fresh, replacements);
    let existing_records = logical_records(parse_jsonl_records(existing)?)?;
    let mut records = parse_jsonl_records(&stable)?;
    let existing_context = fixture_context(existing)?;
    let string_mappings = normalized_string_mappings(
        &records,
        &fresh_records,
        &existing_records,
        fresh_context,
        &existing_context,
    )?;
    let mut existing_index = 0;
    let mut previous_event_time = None::<Value>;
    for (index, record) in records.iter_mut().enumerate() {
        let existing_record = existing_records.get(existing_index);
        let member_count = packed_times(record)?.map_or(1, |times| times.len());
        let inserted_title = record_type(record) == Some("session/title")
            && existing_record.and_then(record_type) != Some("session/title");
        if inserted_title {
            let Some(time) = previous_event_time.clone() else {
                anyhow::bail!("acp-snapshot: inserted title has no preceding event time");
            };
            record.insert("time".to_owned(), time);
        } else {
            if let (Some(mappings), Some(existing_record)) =
                (string_mappings.as_ref(), existing_record)
                && member_count == 1
                && record_type(existing_record) == record_type(record)
            {
                let normalized_fresh = normalized_refresh_record(
                    fresh_records.get(index).ok_or_else(|| {
                        anyhow::anyhow!("acp-snapshot: missing aligned fresh record")
                    })?,
                    fresh_context,
                )?;
                let normalized_existing =
                    normalized_refresh_record(existing_record, &existing_context)?;
                let preserved = preserve_normalized_volatiles(
                    &Value::Object(record.clone()),
                    &Value::Object(existing_record.clone()),
                    &Value::Object(normalized_fresh),
                    &Value::Object(normalized_existing),
                    mappings,
                );
                *record = preserved.as_object().cloned().ok_or_else(|| {
                    anyhow::anyhow!("acp-snapshot: preserved record must be an object")
                })?;
            }
            let existing_start = existing_index.min(existing_records.len());
            let existing_end = existing_records
                .len()
                .min(existing_index.saturating_add(member_count));
            preserve_packed_member_times(record, &existing_records[existing_start..existing_end])?;
            preserve_fixture_volatiles(record, existing_record);
            existing_index += member_count;
        }
        if record.get("time").is_some_and(Value::is_number) {
            previous_event_time = record.get("time").cloned();
        }
    }
    let lines = records
        .iter()
        .map(|record| serde_json::to_string(&Value::Object(record.clone())))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(format!("{}\n", lines.join("\n")))
}

fn initial_system_prompt_snapshot(snapshot: &str) -> &str {
    snapshot
        .find("\n<!-- request/header change ")
        .map_or(snapshot, |marker| &snapshot[..marker])
}

fn child_tool_schemas_snapshot(index: usize) -> String {
    format!("tool-schemas.{index}.expected.json")
}

fn child_system_prompt_snapshot(index: usize) -> String {
    format!("system-prompt.{index}.expected.md")
}

fn child_session_name_re() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    VALUE.get_or_init(|| {
        Regex::new(r"^session\.([1-9][0-9]*)\.jsonl$")
            .expect("static child Session fixture expression is valid")
    })
}

fn child_tool_schema_name_re() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    VALUE.get_or_init(|| {
        Regex::new(r"^tool-schemas\.([1-9][0-9]*)\.expected\.json$")
            .expect("static child tool-schema sidecar expression is valid")
    })
}

fn child_system_prompt_name_re() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    VALUE.get_or_init(|| {
        Regex::new(r"^system-prompt\.([1-9][0-9]*)\.expected\.md$")
            .expect("static child system-prompt sidecar expression is valid")
    })
}

fn uuid_re() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    VALUE.get_or_init(|| {
        Regex::new(r"(?i)^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")
            .expect("static UUID expression is valid")
    })
}

fn parse_jsonl_values(text: &str) -> anyhow::Result<Vec<Value>> {
    text.split('\n')
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).map_err(anyhow::Error::from))
        .collect()
}

fn parse_jsonl_records(text: &str) -> anyhow::Result<Vec<Map<String, Value>>> {
    parse_jsonl_values(text)?
        .into_iter()
        .map(|value| {
            value
                .as_object()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("acp-snapshot: JSONL record must be an object"))
        })
        .collect()
}

fn record_type(record: &Map<String, Value>) -> Option<&str> {
    record.get("type").and_then(Value::as_str)
}

fn complete_message(value: &Value) -> Option<&Map<String, Value>> {
    let object = value.as_object()?;
    (object
        .get("id")
        .and_then(Value::as_str)
        .is_some_and(|id| uuid_re().is_match(id))
        && object.get("role").is_some_and(Value::is_string)
        && object.get("content").is_some_and(Value::is_array)
        && object.get("source").is_some_and(Value::is_object))
    .then_some(object)
}

fn surface_event_message(
    record: &Map<String, Value>,
) -> anyhow::Result<Option<&Map<String, Value>>> {
    let Some(event_type) = record_type(record) else {
        return Ok(None);
    };
    if !is_surface_eligible_type(event_type) {
        return Ok(None);
    }
    let Some(data) = record.get("data").and_then(Value::as_object) else {
        return Ok(None);
    };
    let message = match event_type {
        "user/message" => record.get("data"),
        "assistant/message" | "tool/result" => data.get("message"),
        _ => anyhow::bail!("acp-snapshot: unsupported surface event type {event_type:?}"),
    };
    Ok(message.and_then(complete_message))
}

fn record_messages(record: &Map<String, Value>) -> anyhow::Result<Vec<&Map<String, Value>>> {
    if let Some(message) = surface_event_message(record)? {
        return Ok(vec![message]);
    }
    if record_type(record) != Some("agent/inbox/spliced") {
        return Ok(Vec::new());
    }
    Ok(record
        .get("data")
        .and_then(Value::as_object)
        .and_then(|data| data.get("inserted"))
        .and_then(Value::as_array)
        .map_or_else(Vec::new, |messages| {
            messages.iter().filter_map(complete_message).collect()
        }))
}

fn canonical_json(value: &Value) -> anyhow::Result<String> {
    match value {
        Value::Array(values) => Ok(format!(
            "[{}]",
            values
                .iter()
                .map(canonical_json)
                .collect::<anyhow::Result<Vec<_>>>()?
                .join(",")
        )),
        Value::Object(values) => {
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort();
            Ok(format!(
                "{{{}}}",
                keys.into_iter()
                    .map(|key| {
                        Ok(format!(
                            "{}:{}",
                            serde_json::to_string(key)?,
                            canonical_json(&values[key])?
                        ))
                    })
                    .collect::<anyhow::Result<Vec<_>>>()?
                    .join(",")
            ))
        }
        _ => Ok(serde_json::to_string(value)?),
    }
}

fn unique_message_ids(logs: &[String]) -> anyhow::Result<BTreeMap<String, String>> {
    let mut fingerprints_by_id = BTreeMap::<String, BTreeSet<String>>::new();
    let mut ids_by_fingerprint = BTreeMap::<String, BTreeSet<String>>::new();
    for log in logs {
        for record in parse_jsonl_records(log)? {
            for message in record_messages(&record)? {
                let message_id = message
                    .get("id")
                    .and_then(Value::as_str)
                    .expect("complete message has a string id")
                    .to_owned();
                let mut without_id = message.clone();
                without_id.shift_remove("id");
                let fingerprint = canonical_json(&Value::Object(without_id))?;
                fingerprints_by_id
                    .entry(message_id.clone())
                    .or_default()
                    .insert(fingerprint.clone());
                ids_by_fingerprint
                    .entry(fingerprint)
                    .or_default()
                    .insert(message_id);
            }
        }
    }
    let mut unique = BTreeMap::new();
    for (id, fingerprints) in fingerprints_by_id {
        if fingerprints.len() != 1 {
            continue;
        }
        let fingerprint = fingerprints
            .into_iter()
            .next()
            .expect("single fingerprint exists");
        if ids_by_fingerprint.get(&fingerprint).map(BTreeSet::len) != Some(1) {
            continue;
        }
        unique.insert(fingerprint, id);
    }
    Ok(unique)
}

fn fixture_message_id_replacements(
    logs: &[String],
    fixtures: &[String],
) -> anyhow::Result<BTreeMap<String, String>> {
    let fresh_ids = unique_message_ids(logs)?;
    let existing_ids = unique_message_ids(fixtures)?;
    Ok(fresh_ids
        .into_iter()
        .filter_map(|(fingerprint, fresh)| {
            let existing = existing_ids.get(&fingerprint)?;
            (fresh != *existing).then(|| (fresh, existing.clone()))
        })
        .collect())
}

fn apply_fixture_replacements(content: &str, replacements: &[FixtureReplacement]) -> String {
    replacements
        .iter()
        .fold(content.to_owned(), |stable, replacement| {
            stable.replace(&replacement.from, &replacement.to)
        })
}

fn apply_fixture_message_ids(
    content: &str,
    replacements: &BTreeMap<String, String>,
) -> anyhow::Result<String> {
    content
        .split('\n')
        .map(|line| {
            if line.trim().is_empty() {
                return Ok(line.to_owned());
            }
            let mut record: Value = serde_json::from_str(line)?;
            let changed = rewrite_record_message_ids(&mut record, replacements)?;
            if changed {
                Ok(serde_json::to_string(&record)?)
            } else {
                Ok(line.to_owned())
            }
        })
        .collect::<anyhow::Result<Vec<_>>>()
        .map(|lines| lines.join("\n"))
}

fn rewrite_record_message_ids(
    record: &mut Value,
    replacements: &BTreeMap<String, String>,
) -> anyhow::Result<bool> {
    let Some(object) = record.as_object_mut() else {
        anyhow::bail!("acp-snapshot: JSONL record must be an object");
    };
    let event_type = object
        .get("type")
        .and_then(Value::as_str)
        .map(str::to_owned);
    if let Some(event_type) = event_type.as_deref()
        && is_surface_eligible_type(event_type)
    {
        let message = match event_type {
            "user/message" => object.get_mut("data"),
            "assistant/message" | "tool/result" => object
                .get_mut("data")
                .and_then(Value::as_object_mut)
                .and_then(|data| data.get_mut("message")),
            _ => anyhow::bail!("acp-snapshot: unsupported surface event type {event_type:?}"),
        };
        return Ok(
            message.is_some_and(|message| replace_complete_message_id(message, replacements))
        );
    }
    if event_type.as_deref() != Some("agent/inbox/spliced") {
        return Ok(false);
    }
    let Some(inserted) = object
        .get_mut("data")
        .and_then(Value::as_object_mut)
        .and_then(|data| data.get_mut("inserted"))
        .and_then(Value::as_array_mut)
    else {
        return Ok(false);
    };
    let mut changed = false;
    for message in inserted {
        changed |= replace_complete_message_id(message, replacements);
    }
    Ok(changed)
}

fn replace_complete_message_id(
    message: &mut Value,
    replacements: &BTreeMap<String, String>,
) -> bool {
    let Some(current) = complete_message(message)
        .and_then(|message| message.get("id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return false;
    };
    let Some(replacement) = replacements.get(&current) else {
        return false;
    };
    message
        .as_object_mut()
        .expect("complete message is an object")
        .insert("id".to_owned(), Value::String(replacement.clone()));
    true
}

fn packed_times(record: &Map<String, Value>) -> anyhow::Result<Option<Vec<f64>>> {
    if !matches!(
        record_type(record),
        Some("text-chunks" | "reasoning-chunks" | "tool-call-chunks")
    ) {
        return Ok(None);
    }
    let mut times = vec![
        record
            .get("time0")
            .and_then(Value::as_f64)
            .ok_or_else(|| anyhow::anyhow!("acp-snapshot: packed row has no numeric time0"))?,
    ];
    let gaps = record
        .get("data")
        .and_then(Value::as_object)
        .and_then(|data| data.get("dt"))
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("acp-snapshot: packed row has no dt array"))?;
    for gap in gaps {
        let gap = gap
            .as_f64()
            .ok_or_else(|| anyhow::anyhow!("acp-snapshot: packed row has a non-numeric gap"))?;
        times.push(times.last().copied().unwrap_or_default() + gap);
    }
    Ok(Some(times))
}

fn logical_records(records: Vec<Map<String, Value>>) -> anyhow::Result<Vec<Map<String, Value>>> {
    let mut logical = Vec::new();
    for record in records {
        let Some(times) = packed_times(&record)? else {
            logical.push(record);
            continue;
        };
        for time in times {
            let mut event = Map::new();
            event.insert(
                "type".to_owned(),
                Value::String("assistant/chunk".to_owned()),
            );
            event.insert("time".to_owned(), js_number_value(time));
            logical.push(event);
        }
    }
    Ok(logical)
}

fn preserve_fixture_volatiles(
    record: &mut Map<String, Value>,
    existing: Option<&Map<String, Value>>,
) {
    let Some(existing) = existing.filter(|existing| record_type(existing) == record_type(record))
    else {
        return;
    };
    if record_type(record) == Some("session") {
        for field in ["id", "createdAt", "cwd", "parentSession"] {
            if record.contains_key(field)
                && let Some(value) = existing.get(field)
            {
                record.insert(field.to_owned(), value.clone());
            }
        }
        return;
    }
    if record.contains_key("time")
        && let Some(time) = existing.get("time")
    {
        record.insert("time".to_owned(), time.clone());
    }
    if record_type(record) != Some("hook/result") {
        return;
    }
    if let (Some(data), Some(existing_data)) = (
        record.get_mut("data").and_then(Value::as_object_mut),
        existing.get("data").and_then(Value::as_object),
    ) && data.contains_key("durationMs")
        && let Some(duration) = existing_data.get("durationMs")
    {
        data.insert("durationMs".to_owned(), duration.clone());
    }
}

fn preserve_packed_member_times(
    record: &mut Map<String, Value>,
    existing_members: &[Map<String, Value>],
) -> anyhow::Result<()> {
    if packed_times(record)?.is_none() {
        return Ok(());
    }
    let Some(first_time) = existing_members
        .first()
        .and_then(|member| member.get("time"))
        .and_then(safe_integer)
    else {
        return Ok(());
    };
    record.insert("time0".to_owned(), Value::from(first_time));
    let gap_count = record
        .get("data")
        .and_then(Value::as_object)
        .and_then(|data| data.get("dt"))
        .and_then(Value::as_array)
        .map(Vec::len)
        .ok_or_else(|| anyhow::anyhow!("acp-snapshot: packed row has no dt array"))?;
    if existing_members.len() != gap_count + 1 {
        return Ok(());
    }
    let Some(times) = existing_members
        .iter()
        .map(|member| member.get("time").and_then(safe_integer))
        .collect::<Option<Vec<_>>>()
    else {
        return Ok(());
    };
    let gaps = times
        .windows(2)
        .map(|pair| pair[1].checked_sub(pair[0]))
        .collect::<Option<Vec<_>>>();
    let Some(gaps) = gaps.filter(|gaps| gaps.iter().all(|gap| is_safe_integer(*gap))) else {
        return Ok(());
    };
    record
        .get_mut("data")
        .and_then(Value::as_object_mut)
        .expect("validated packed row has object data")
        .insert(
            "dt".to_owned(),
            Value::Array(gaps.into_iter().map(Value::from).collect()),
        );
    Ok(())
}

fn preserve_normalized_volatiles(
    fresh: &Value,
    existing: &Value,
    normalized_fresh: &Value,
    normalized_existing: &Value,
    string_mappings: &BTreeMap<String, String>,
) -> Value {
    match (fresh, existing, normalized_fresh, normalized_existing) {
        (
            Value::Array(fresh),
            Value::Array(existing),
            Value::Array(normalized_fresh),
            Value::Array(normalized_existing),
        ) if fresh.len() == existing.len()
            && fresh.len() == normalized_fresh.len()
            && fresh.len() == normalized_existing.len() =>
        {
            Value::Array(
                fresh
                    .iter()
                    .enumerate()
                    .map(|(index, value)| {
                        preserve_normalized_volatiles(
                            value,
                            &existing[index],
                            &normalized_fresh[index],
                            &normalized_existing[index],
                            string_mappings,
                        )
                    })
                    .collect(),
            )
        }
        (
            Value::Object(fresh),
            Value::Object(existing),
            Value::Object(normalized_fresh),
            Value::Object(normalized_existing),
        ) => Value::Object(
            fresh
                .iter()
                .map(|(key, value)| {
                    let preserved = match (
                        existing.get(key),
                        normalized_fresh.get(key),
                        normalized_existing.get(key),
                    ) {
                        (Some(existing), Some(normalized_fresh), Some(normalized_existing)) => {
                            preserve_normalized_volatiles(
                                value,
                                existing,
                                normalized_fresh,
                                normalized_existing,
                                string_mappings,
                            )
                        }
                        _ => value.clone(),
                    };
                    (key.clone(), preserved)
                })
                .collect(),
        ),
        (
            Value::String(fresh),
            Value::String(existing),
            Value::String(normalized_fresh),
            Value::String(normalized_existing),
        ) if normalized_fresh == normalized_existing => {
            let key = serde_json::to_string(&[normalized_fresh, fresh])
                .expect("strings always serialize");
            if string_mappings.get(&key) == Some(existing) {
                Value::String(existing.clone())
            } else {
                Value::String(fresh.clone())
            }
        }
        _ if json_object_is(normalized_fresh, normalized_existing) => existing.clone(),
        _ => fresh.clone(),
    }
}

fn normalized_refresh_record(
    record: &Map<String, Value>,
    context: &NormalizeContext,
) -> anyhow::Result<Map<String, Value>> {
    let line = format!(
        "{}\n",
        serde_json::to_string(&Value::Object(record.clone()))?
    );
    let normalized = normalize_session_log(&line, context, NormalizeOptions::default())?;
    parse_jsonl_records(&normalized)?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("acp-snapshot: normalization removed a record"))
}

#[allow(clippy::too_many_arguments)]
fn collect_normalized_string_mappings(
    fresh: &Value,
    existing: &Value,
    normalized_fresh: &Value,
    normalized_existing: &Value,
    excluded_strings: &BTreeSet<String>,
    forward: &mut BTreeMap<String, String>,
    reverse: &mut BTreeMap<String, String>,
) -> bool {
    match (fresh, existing, normalized_fresh, normalized_existing) {
        (
            Value::Array(fresh),
            Value::Array(existing),
            Value::Array(normalized_fresh),
            Value::Array(normalized_existing),
        ) => {
            if fresh.len() != existing.len()
                || fresh.len() != normalized_fresh.len()
                || fresh.len() != normalized_existing.len()
            {
                return true;
            }
            fresh.iter().enumerate().all(|(index, value)| {
                collect_normalized_string_mappings(
                    value,
                    &existing[index],
                    &normalized_fresh[index],
                    &normalized_existing[index],
                    excluded_strings,
                    forward,
                    reverse,
                )
            })
        }
        (
            Value::Object(fresh),
            Value::Object(existing),
            Value::Object(normalized_fresh),
            Value::Object(normalized_existing),
        ) => fresh.iter().all(|(key, value)| {
            match (
                existing.get(key),
                normalized_fresh.get(key),
                normalized_existing.get(key),
            ) {
                (Some(existing), Some(normalized_fresh), Some(normalized_existing)) => {
                    collect_normalized_string_mappings(
                        value,
                        existing,
                        normalized_fresh,
                        normalized_existing,
                        excluded_strings,
                        forward,
                        reverse,
                    )
                }
                _ => true,
            }
        }),
        (
            Value::String(fresh),
            Value::String(existing),
            Value::String(normalized_fresh),
            Value::String(normalized_existing),
        ) if normalized_fresh == normalized_existing
            && fresh != existing
            && !excluded_strings.contains(fresh)
            && !excluded_strings.contains(existing) =>
        {
            let fresh_key = serde_json::to_string(&[normalized_fresh, fresh])
                .expect("strings always serialize");
            let existing_key = serde_json::to_string(&[normalized_fresh, existing])
                .expect("strings always serialize");
            if forward
                .get(&fresh_key)
                .is_some_and(|mapped| mapped != existing)
                || reverse
                    .get(&existing_key)
                    .is_some_and(|mapped| mapped != fresh)
            {
                return false;
            }
            forward.insert(fresh_key, existing.clone());
            reverse.insert(existing_key, fresh.clone());
            true
        }
        _ => true,
    }
}

fn normalized_string_mappings(
    records: &[Map<String, Value>],
    fresh_records: &[Map<String, Value>],
    existing_records: &[Map<String, Value>],
    fresh_context: &NormalizeContext,
    existing_context: &NormalizeContext,
) -> anyhow::Result<Option<BTreeMap<String, String>>> {
    let mut excluded_strings = BTreeSet::new();
    for record in fresh_records.iter().chain(existing_records) {
        for message in record_messages(record)? {
            if let Some(id) = message.get("id").and_then(Value::as_str) {
                excluded_strings.insert(id.to_owned());
            }
        }
    }
    let mut forward = BTreeMap::new();
    let mut reverse = BTreeMap::new();
    let mut existing_index = 0;
    for (record_index, record) in records.iter().enumerate() {
        let existing_record = existing_records.get(existing_index);
        let member_count = packed_times(record)?.map_or(1, |times| times.len());
        if record_type(record) == Some("session/title")
            && existing_record.and_then(record_type) != Some("session/title")
        {
            continue;
        }
        if member_count > 1 {
            let existing_start = existing_index.min(existing_records.len());
            let existing_end = existing_records
                .len()
                .min(existing_index.saturating_add(member_count));
            let existing_members = &existing_records[existing_start..existing_end];
            if existing_members.len() != member_count
                || existing_members
                    .iter()
                    .any(|member| record_type(member) != Some("assistant/chunk"))
            {
                return Ok(None);
            }
        } else {
            let Some(existing_record) = existing_record else {
                return Ok(None);
            };
            if record_type(existing_record) != record_type(record) {
                return Ok(None);
            }
            let normalized_fresh = normalized_refresh_record(
                fresh_records
                    .get(record_index)
                    .ok_or_else(|| anyhow::anyhow!("acp-snapshot: missing aligned fresh record"))?,
                fresh_context,
            )?;
            let normalized_existing = normalized_refresh_record(existing_record, existing_context)?;
            if !collect_normalized_string_mappings(
                &Value::Object(record.clone()),
                &Value::Object(existing_record.clone()),
                &Value::Object(normalized_fresh),
                &Value::Object(normalized_existing),
                &excluded_strings,
                &mut forward,
                &mut reverse,
            ) {
                return Ok(None);
            }
        }
        existing_index += member_count;
    }
    Ok((existing_index == existing_records.len()).then_some(forward))
}

fn safe_integer(value: &Value) -> Option<i64> {
    let number = value.as_f64()?;
    if number.fract() != 0.0
        || !(-9_007_199_254_740_991.0..=9_007_199_254_740_991.0).contains(&number)
    {
        return None;
    }
    format!("{number:.0}").parse().ok()
}

fn is_safe_integer(value: i64) -> bool {
    (-9_007_199_254_740_991..=9_007_199_254_740_991).contains(&value)
}

fn json_object_is(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Array(_) | Value::Object(_), _) | (_, Value::Array(_) | Value::Object(_)) => false,
        (Value::Number(left), Value::Number(right)) => match (left.as_f64(), right.as_f64()) {
            (Some(left), Some(right)) => left.to_bits() == right.to_bits(),
            _ => left == right,
        },
        _ => left == right,
    }
}

fn js_number_value(number: f64) -> Value {
    if number.fract() == 0.0
        && (-9_007_199_254_740_991.0..=9_007_199_254_740_991.0).contains(&number)
        && let Ok(integer) = format!("{number:.0}").parse::<i64>()
    {
        return Value::from(integer);
    }
    Value::from(number)
}
