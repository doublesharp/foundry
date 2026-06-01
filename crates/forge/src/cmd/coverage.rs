use super::{
    test::{ProjectPathsAwareFilter, TestArgs, TestExecutionOptions},
    watch::WatchArgs,
};
use crate::coverage::{
    BytecodeReporter, ContractId, CoverageAttributionReporter, CoverageItem, CoverageItemKind,
    CoverageReport, CoverageReporter, CoverageSummaryReporter, DebugReporter, InstrumentedHitMaps,
    ItemAnchor, LcovReporter, ResolvedHitMap, ResolvedHitMaps, SourceLocation,
    analysis::{SourceAnalysis, SourceFiles},
    anchors::{find_anchors, find_execution_anchors},
    instrumentation::{
        CoverageInstrumentationPreprocessor, InstrumentedCoverageMetadata,
        InstrumentedCoverageMetadataRef, InstrumentedCoverageProbe, InstrumentedCoverageProbeKind,
        read_metadata,
    },
};
use alloy_json_abi::StateMutability;
use alloy_primitives::{
    Address, Bytes, U256,
    map::{B256HashMap, HashMap},
};
use clap::{Parser, ValueHint};
use eyre::Result;
use foundry_cli::utils::{FoundryPathExt, LoadConfig, STATIC_FUZZ_SEED};
use foundry_common::{
    TestFilter,
    compile::{ProjectCompiler, with_compilation_reporter},
    errors::convert_solar_errors,
};
use foundry_compilers::{
    Artifact, ArtifactId, Project, ProjectCompileOutput, ProjectPathsConfig, VYPER_EXTENSIONS,
    artifacts::{CompactBytecode, CompactDeployedBytecode, sourcemap::SourceMap},
    compilers::{Language, multi::MultiCompilerLanguage},
    project::ProjectCompiler as FoundryProjectCompiler,
    utils::source_files_iter,
};
use foundry_config::{
    Config, CoverageConfig, CoverageReportKind, InlineConfig, parse_lcov_version,
};
use foundry_evm::{core::ic::IcPcMap, opts::EvmOpts};
use globset::{Glob, GlobSetBuilder};
use rayon::prelude::*;
use semver::Version;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

// Loads project's figment and merges the build cli arguments into it
foundry_config::impl_figment_convert!(CoverageArgs, test);

/// CLI arguments for `forge coverage`.
///
/// Most flags here have a corresponding `[profile.<name>.coverage]` config
/// option in `foundry.toml`. CLI flags take precedence over config; the helper
/// `resolve_with` merges them after the config is loaded.
#[derive(Parser)]
#[command(after_long_help = r#"Source attribution:
  Coverage follows compiler source maps. Inherited modifier code is reported under the
  source where the modifier is declared. Dependency sources are excluded by default;
  use `--include-libs` to include their coverage.

Compatibility:
  `forge coverage` supports test filters and `--watch`, but not test-only output or
  execution modes such as `--json`, `--junit`, `--list`, `--debug`, flame profiles,
  symbolic artifact replay, showmap replay, brutalization, or mutation testing. Use
  `--report lcov` for interoperable coverage data or `--report attribution` for
  Foundry's per-test JSON attribution report."#)]
pub struct CoverageArgs {
    /// The report type to use for coverage.
    ///
    /// This flag can be used multiple times. Falls back to the
    /// `[profile.<name>.coverage] report` config value when not provided
    /// (default: `summary`).
    #[arg(long, value_enum)]
    report: Vec<CoverageReportKind>,

    /// The version of the LCOV "tracefile" format to use.
    ///
    /// Format: `MAJOR[.MINOR]`.
    ///
    /// Main differences:
    /// - `1.x`: The original v1 format.
    /// - `2.0`: Adds support for "line end" numbers for functions.
    /// - `2.2`: Changes the format of functions.
    ///
    /// Falls back to the `[profile.<name>.coverage] lcov_version` config value
    /// when not provided.
    #[arg(long = "lcov-version", value_parser = parse_lcov_version)]
    lcov_version_cli: Option<Version>,

    /// The resolved LCOV version to use after merging CLI and config values.
    #[arg(skip = Version::new(1, 0, 0))]
    lcov_version: Version,

    /// Enable viaIR with minimum optimization
    ///
    /// This can fix most of the "stack too deep" errors while resulting a
    /// relatively accurate source map.
    #[arg(long)]
    ir_minimum: bool,

    /// Collect coverage by instrumenting sources before compilation instead of from source maps.
    ///
    /// Probes are injected into the source and recorded at runtime, so line, statement, branch,
    /// and function coverage stay accurate even when the optimizer or `viaIR` is enabled. Unlike
    /// the default mode, this respects the project's optimizer and `viaIR` settings rather than
    /// disabling them, letting coverage reflect the build you actually ship.
    #[arg(long)]
    instrumented: bool,

    /// The path to output the report.
    ///
    /// Used only when a single file report is requested. If not specified, the
    /// report will be stored in the root of the project.
    #[arg(
        long,
        value_hint = ValueHint::FilePath,
        value_name = "PATH"
    )]
    report_file: Option<PathBuf>,

    /// Include dependency sources in the coverage report.
    #[arg(long)]
    include_libs: bool,

    /// Whether to exclude tests from the coverage report.
    #[arg(long)]
    exclude_tests: bool,

    /// The coverage reporters to use. Constructed from the other fields.
    #[arg(skip)]
    reporters: Vec<Box<dyn CoverageReporter>>,

    /// Glob patterns of source files to exclude from the coverage report.
    /// Populated from `[profile.<name>.coverage] skip_files` after config is
    /// loaded; not exposed directly on the CLI.
    #[arg(skip)]
    skip_files: Vec<String>,

    #[command(flatten)]
    test: TestArgs,
}

impl CoverageArgs {
    fn report_path(&self, root: &Path, default_file_name: &str) -> PathBuf {
        let report_file =
            (self.file_report_count() == 1).then_some(self.report_file.as_deref()).flatten();
        root.join(report_file.unwrap_or_else(|| Path::new(default_file_name)))
    }

    fn file_report_count(&self) -> usize {
        let has_lcov = self.report.iter().any(|kind| matches!(kind, CoverageReportKind::Lcov));
        let has_attribution =
            self.report.iter().any(|kind| matches!(kind, CoverageReportKind::Attribution));
        usize::from(has_lcov) + usize::from(has_attribution)
    }

    pub(crate) fn ensure_mode_compatible(&self) -> Result<()> {
        self.test.ensure_coverage_mode_compatible()
    }

    pub async fn run(mut self) -> Result<()> {
        self.ensure_mode_compatible()?;

        let (mut config, evm_opts) = self.load_config_and_evm_opts()?;

        // install missing dependencies
        self.install_missing_dependencies(&mut config)?;

        // Default to a static fuzz seed so coverage reports are deterministic,
        // but allow the user to override it via `--fuzz-seed` or `[fuzz] seed` in config.
        if config.fuzz.seed.is_none() {
            config.fuzz.seed = Some(U256::from_be_bytes(STATIC_FUZZ_SEED));
        }

        // Merge CLI args with `[profile.<name>.coverage]` config values. CLI
        // flags take precedence; unset CLI flags fall back to the config.
        self.resolve_with(&config.coverage);
        let filter = self.test.filter(&config)?;

        if self.instrumented && self.report.contains(&CoverageReportKind::Bytecode) {
            eyre::bail!("`--report bytecode` is not supported with `--instrumented`");
        }

        let (paths, mut output, instrumentation) = {
            let build = self.build(&config, &filter)?;
            (build.project.paths, build.output, build.instrumentation)
        };

        if self.report_file.is_some() && self.file_report_count() > 1 {
            sh_warn!(
                "`--report-file` is ignored when multiple file reports are requested; \
                 each report will use its default output path"
            )?;
        }

        self.populate_reporters(&paths.root);

        sh_println!("Analysing contracts...")?;
        let (report, instrumented_index) =
            self.prepare(&paths, &mut output, instrumentation.as_ref())?;

        sh_println!("Running tests...")?;
        self.collect(&paths.root, &output, report, instrumented_index, config, evm_opts, filter)
            .await
    }

    /// Merge `[profile.<name>.coverage]` config values into this struct. CLI
    /// flags already set on `self` win; unset/false flags inherit from
    /// `config`.
    ///
    /// After this returns:
    /// - `self.report` is non-empty.
    /// - boolean flags reflect `cli || config` (CLI cannot disable a flag set to `true` in config;
    ///   this matches the pre-existing flag-only semantics where booleans defaulted to `false`).
    fn resolve_with(&mut self, config: &CoverageConfig) {
        if self.report.is_empty() {
            self.report.clone_from(&config.report);
        }
        self.lcov_version =
            self.lcov_version_cli.clone().unwrap_or_else(|| config.lcov_version.clone());
        if !self.ir_minimum {
            self.ir_minimum = config.ir_minimum;
        }
        if self.report_file.is_none() {
            self.report_file.clone_from(&config.report_file);
        }
        if !self.include_libs {
            self.include_libs = config.include_libs;
        }
        if !self.exclude_tests {
            self.exclude_tests = config.exclude_tests;
        }
        // Glob filters are additive — there's no CLI flag for these, so always
        // take from config.
        self.skip_files.clone_from(&config.skip_files);
    }

    fn populate_reporters(&mut self, root: &Path) {
        self.reporters = self
            .report
            .iter()
            .filter_map(|report_kind| match report_kind {
                CoverageReportKind::Summary => {
                    Some(Box::<CoverageSummaryReporter>::default() as Box<dyn CoverageReporter>)
                }
                CoverageReportKind::Lcov => {
                    let path = self.report_path(root, "lcov.info");
                    Some(Box::new(LcovReporter::new(path, self.lcov_version.clone())))
                }
                CoverageReportKind::Bytecode => Some(Box::new(BytecodeReporter::new(
                    root.to_path_buf(),
                    root.join("bytecode-coverage"),
                ))),
                CoverageReportKind::Debug => Some(Box::new(DebugReporter)),
                CoverageReportKind::Attribution => None,
            })
            .collect::<Vec<_>>();
    }

    /// Builds the project.
    fn build(&self, config: &Config, filter: &ProjectPathsAwareFilter) -> Result<CoverageBuild> {
        let mut project = config.ephemeral_project()?;

        if self.instrumented {
            if self.ir_minimum {
                sh_warn!(
                    "`--ir-minimum` enables `viaIR` with minimum optimization for the \
                     instrumented coverage build."
                )?;
                config.disable_optimizations(&mut project, true);
            }
        } else {
            if self.ir_minimum {
                sh_warn!(
                    "`--ir-minimum` enables `viaIR` with minimum optimization, \
                     which can result in inaccurate source mappings.\n\
                     Only use this flag as a workaround if you are experiencing \"stack too deep\" errors.\n\
                     Note that `viaIR` is production ready since Solidity 0.8.13 and above.\n\
                     See more: https://book.getfoundry.sh/guides/best-practices/stack-too-deep"
                )?;
            } else {
                sh_warn!(
                    "optimizer settings and `viaIR` have been disabled for accurate coverage reports.\n\
                     If you encounter \"stack too deep\" errors, consider using `--ir-minimum` which \
                     enables `viaIR` with minimum optimization resolving most of the errors.\n\
                     See more: https://book.getfoundry.sh/guides/best-practices/stack-too-deep"
                )?;
            }
            config.disable_optimizations(&mut project, self.ir_minimum);
        }

        let filtered_sources = if filter.args().path_pattern.is_some()
            || filter.args().path_pattern_inverse.is_some()
        {
            Some(
                source_files_iter(&config.src, MultiCompilerLanguage::FILE_EXTENSIONS)
                    .chain(
                        source_files_iter(&config.test, MultiCompilerLanguage::FILE_EXTENSIONS)
                            // Preserve path-filter behavior for conventional test files while
                            // still scanning non-test fixtures under the test root.
                            .filter(|path| !path.is_sol_test() || filter.matches_path(path)),
                    )
                    // Coverage reports include scripts even though they are not test targets.
                    .chain(source_files_iter(
                        &config.script,
                        MultiCompilerLanguage::FILE_EXTENSIONS,
                    ))
                    .collect::<BTreeSet<_>>(),
            )
        } else {
            None
        };

        let (output, instrumentation) = if self.instrumented {
            let metadata = Arc::new(Mutex::new(InstrumentedCoverageMetadata::default()));
            let output = self
                .compile_instrumented(
                    &project,
                    metadata.clone(),
                    filter.args().coverage_pattern_inverse.clone(),
                    filtered_sources.as_ref(),
                )?
                .with_stripped_file_prefixes(project.root());
            (output, Some(read_metadata(&metadata)?))
        } else {
            let mut compiler =
                ProjectCompiler::new().dynamic_test_linking(config.dynamic_test_linking);
            if let Some(sources) = filtered_sources {
                compiler = compiler.files(sources);
            }
            let output = compiler.compile(&project)?.with_stripped_file_prefixes(project.root());
            (output, None)
        };

        Ok(CoverageBuild { project, output, instrumentation })
    }

    fn compile_instrumented(
        &self,
        project: &Project,
        metadata: InstrumentedCoverageMetadataRef,
        excluded_sources: Option<regex::Regex>,
        filtered_sources: Option<&BTreeSet<PathBuf>>,
    ) -> Result<ProjectCompileOutput> {
        let root = project.root().to_path_buf();
        let output = with_compilation_reporter(false, Some(root), || {
            let mut sources = project.paths.read_input_files()?;
            if let Some(filtered_sources) = filtered_sources {
                sources.retain(|path, _| filtered_sources.contains(path));
            }
            let compiler = FoundryProjectCompiler::with_sources(project, sources)?
                .with_preprocessor(CoverageInstrumentationPreprocessor::new(
                    metadata,
                    self.include_libs,
                    self.exclude_tests,
                    excluded_sources,
                ));
            compiler.compile().map_err(eyre::Report::from)
        })?;

        if output.has_compiler_errors() {
            eyre::bail!("{output}");
        }
        if output.is_unchanged() {
            sh_println!("No files changed, compilation skipped")?;
        } else {
            sh_println!("{output}")?;
        }

        Ok(output)
    }

    /// Builds the coverage report.
    #[instrument(name = "Coverage::prepare", skip_all)]
    fn prepare(
        &self,
        project_paths: &ProjectPathsConfig,
        output: &mut ProjectCompileOutput,
        instrumentation: Option<&InstrumentedCoverageMetadata>,
    ) -> Result<(CoverageReport, Option<InstrumentedCoverageTagIndex>)> {
        if let Some(metadata) = instrumentation {
            return self.prepare_instrumented(project_paths, output, metadata);
        }

        let mut report = CoverageReport::default();

        output.parser_mut().solc_mut().compiler_mut().enter_mut(|compiler| {
            if compiler.gcx().stage() < Some(solar::config::CompilerStage::Lowering) {
                let _ = compiler.lower_asts();
            }
            convert_solar_errors(compiler.dcx())
        })?;
        let output = &*output;

        // Collect source files.
        let mut sources_by_build = HashMap::<String, SourceFiles>::default();
        for (path, sources) in &output.output().sources.0 {
            // Filter out vyper sources.
            if path
                .extension()
                .and_then(|s| s.to_str())
                .is_some_and(|ext| VYPER_EXTENSIONS.contains(&ext))
            {
                continue;
            }

            for source in sources {
                let source_file = &source.source_file;
                report.add_source(source.build_id.clone(), source_file.id as usize, path.clone());

                // Filter out libs dependencies and tests.
                if (!self.include_libs && project_paths.has_library_ancestor(path))
                    || (self.exclude_tests && project_paths.is_test(path))
                {
                    continue;
                }

                sources_by_build
                    .entry(source.build_id.clone())
                    .or_default()
                    .sources
                    .insert(source_file.id, project_paths.root.join(path));
            }
        }

        // Get source maps and bytecodes.
        let artifacts: Vec<ArtifactData> = output
            .artifact_ids()
            .par_bridge() // This parses source maps, so we want to run it in parallel.
            .filter_map(|(id, artifact)| {
                let source_id = report.get_source_id(&id.build_id, &id.source)?;
                ArtifactData::new(&id, source_id, artifact)
            })
            .collect();

        // Add coverage items.
        for (build_id, sources) in &sources_by_build {
            let source_analysis = SourceAnalysis::new(sources, output)?;
            let anchors = artifacts
                .par_iter()
                .filter(|artifact| artifact.contract_id.build_id == *build_id)
                .map(|artifact| {
                    let creation_code_anchors = artifact.creation.find_anchors(&source_analysis);
                    let deployed_code_anchors = artifact.deployed.find_anchors(&source_analysis);
                    (artifact.contract_id.clone(), (creation_code_anchors, deployed_code_anchors))
                })
                .collect_vec_list();
            report.add_anchors(anchors.into_iter().flatten());
            for artifact in
                artifacts.iter().filter(|artifact| artifact.contract_id.build_id == *build_id)
            {
                let execution_anchors = find_execution_anchors(
                    artifact.contract_id.source_id as u32,
                    &artifact.contract_id.contract_name,
                    &source_analysis,
                );
                report.add_execution_anchors(
                    artifact.contract_id.clone(),
                    execution_anchors,
                    artifact.function_selectors.iter().copied(),
                    artifact.has_receive,
                    artifact.fallback_payable,
                );
            }
            report.add_analysis(build_id.clone(), source_analysis);
        }

        if self.reporters.iter().any(|reporter| reporter.needs_source_maps()) {
            report.add_source_maps(artifacts.into_iter().map(|artifact| {
                (artifact.contract_id, (artifact.creation.source_map, artifact.deployed.source_map))
            }));
        }

        Ok((report, None))
    }

    fn prepare_instrumented(
        &self,
        project_paths: &ProjectPathsConfig,
        output: &ProjectCompileOutput,
        instrumentation: &InstrumentedCoverageMetadata,
    ) -> Result<(CoverageReport, Option<InstrumentedCoverageTagIndex>)> {
        let mut report = CoverageReport::default();
        let mut included_sources = BTreeSet::new();

        for (path, source_file, version) in output.output().sources.sources_with_version() {
            if path
                .extension()
                .and_then(|s| s.to_str())
                .is_some_and(|ext| VYPER_EXTENSIONS.contains(&ext))
            {
                continue;
            }

            report.add_source(version.clone(), source_file.id as usize, path.clone());

            if (!self.include_libs && project_paths.has_library_ancestor(path))
                || (self.exclude_tests && project_paths.is_test(path))
            {
                continue;
            }

            included_sources.insert((version.clone(), path.clone(), source_file.id));
        }

        let mut grouped = BTreeMap::<(Version, u32), Vec<&InstrumentedCoverageProbe>>::new();
        for probe in &instrumentation.probes {
            let Some(source_id) = report.get_source_id(probe.version.clone(), probe.path.clone())
            else {
                continue;
            };
            if included_sources.contains(&(
                probe.version.clone(),
                probe.path.clone(),
                source_id as u32,
            )) {
                grouped.entry((probe.version.clone(), source_id as u32)).or_default().push(probe);
            }
        }

        let mut by_version = HashMap::<Version, Vec<(u32, Vec<CoverageItem>)>>::default();
        for ((version, source_id), probes) in &grouped {
            let items = build_source_items(*source_id, probes);
            by_version.entry(version.clone()).or_default().push((*source_id, items));
        }

        let mut tag_index = InstrumentedCoverageTagIndex::default();
        for (version, sourced_items) in by_version {
            let analysis = SourceAnalysis::from_sourced_items(sourced_items);
            let mut indexed_sources = BTreeSet::new();
            let mut line_item_ids = HashMap::<(u32, u32, u32), u32>::default();
            let mut statement_item_ids = HashMap::<(u32, u32, u32), u32>::default();
            let mut branch_item_ids = HashMap::<(u32, u32, u32), u32>::default();
            let mut function_item_ids = HashMap::<(u32, u32, u32), u32>::default();

            for ((probe_version, source_id), probes) in &grouped {
                if *probe_version != version {
                    continue;
                }
                if indexed_sources.insert(*source_id) {
                    for (item_id, item) in analysis.items_for_source_enumerated(*source_id) {
                        match item.kind {
                            CoverageItemKind::Line => {
                                line_item_ids.insert(
                                    (*source_id, item.loc.lines.start, item.loc.lines.end),
                                    item_id,
                                );
                            }
                            CoverageItemKind::Statement => {
                                statement_item_ids.insert(
                                    (*source_id, item.loc.bytes.start, item.loc.bytes.end),
                                    item_id,
                                );
                            }
                            CoverageItemKind::Branch { branch_id, path_id, .. } => {
                                branch_item_ids.insert((*source_id, branch_id, path_id), item_id);
                            }
                            CoverageItemKind::Function { .. } => {
                                function_item_ids.insert(
                                    (*source_id, item.loc.bytes.start, item.loc.bytes.end),
                                    item_id,
                                );
                            }
                        }
                    }
                }

                for probe in probes {
                    let line_item_id = line_item_ids
                        .get(&(*source_id, probe.lines.start, probe.lines.end))
                        .copied();
                    if let Some(line_item_id) = line_item_id {
                        let statement_item_id = match probe.kind {
                            InstrumentedCoverageProbeKind::Statement
                            | InstrumentedCoverageProbeKind::RequirePre { .. } => {
                                statement_item_ids
                                    .get(&(*source_id, probe.bytes.start, probe.bytes.end))
                                    .copied()
                            }
                            _ => None,
                        };
                        let branch_item_id = match probe.kind {
                            InstrumentedCoverageProbeKind::Branch { branch_id, path_id } => {
                                branch_item_ids.get(&(*source_id, branch_id, path_id)).copied()
                            }
                            _ => None,
                        };
                        let require_branch = match probe.kind {
                            InstrumentedCoverageProbeKind::RequirePre { branch_id } => {
                                let false_item_id =
                                    branch_item_ids.get(&(*source_id, branch_id, 0)).copied();
                                let true_item_id =
                                    branch_item_ids.get(&(*source_id, branch_id, 1)).copied();
                                false_item_id.zip(true_item_id).map(
                                    |(false_item_id, true_item_id)| InstrumentedRequireBranchIds {
                                        role: InstrumentedRequireProbeRole::Pre,
                                        false_item_id,
                                        true_item_id,
                                    },
                                )
                            }
                            InstrumentedCoverageProbeKind::RequirePost { branch_id } => {
                                let false_item_id =
                                    branch_item_ids.get(&(*source_id, branch_id, 0)).copied();
                                let true_item_id =
                                    branch_item_ids.get(&(*source_id, branch_id, 1)).copied();
                                false_item_id.zip(true_item_id).map(
                                    |(false_item_id, true_item_id)| InstrumentedRequireBranchIds {
                                        role: InstrumentedRequireProbeRole::Post,
                                        false_item_id,
                                        true_item_id,
                                    },
                                )
                            }
                            _ => None,
                        };
                        let function_item_id = match probe.kind {
                            InstrumentedCoverageProbeKind::Function { .. } => function_item_ids
                                .get(&(*source_id, probe.bytes.start, probe.bytes.end))
                                .copied(),
                            _ => None,
                        };
                        tag_index.0.insert(
                            probe.tag,
                            InstrumentedCoverageItemIds {
                                version: version.clone(),
                                line_item_id,
                                statement_item_id,
                                branch_item_id,
                                require_branch,
                                function_item_id,
                            },
                        );
                    }
                }
            }
            report.add_analysis(version, analysis);
        }

        Ok((report, Some(tag_index)))
    }

    /// Runs tests, collects coverage data and generates the final report.
    #[instrument(name = "Coverage::collect", skip_all)]
    async fn collect(
        mut self,
        project_root: &Path,
        output: &ProjectCompileOutput,
        mut report: CoverageReport,
        instrumented_index: Option<InstrumentedCoverageTagIndex>,
        config: Config,
        evm_opts: EvmOpts,
        filter: ProjectPathsAwareFilter,
    ) -> Result<()> {
        let inline_config = Arc::new(InlineConfig::new_parsed(output, &config)?);
        let instrumented = instrumented_index.is_some();
        self.test.instrumented_coverage = instrumented;
        let execution = if instrumented {
            TestExecutionOptions::default_run(inline_config)
        } else {
            TestExecutionOptions::coverage(inline_config)
        };
        let outcome =
            self.test.run_tests(project_root, config, evm_opts, output, &filter, execution).await?;

        let known_contracts = outcome.known_contracts.as_ref().unwrap();
        let mut resolved_hit_maps = ResolvedHitMaps::default();

        if let Some(index) = instrumented_index {
            let mut aggregated_hits = InstrumentedHitMaps::default();
            for suite in outcome.results.values() {
                for result in suite.test_results.values() {
                    if let Some(hits) = &result.instrumented_coverage {
                        aggregated_hits.merge_ref(hits);
                    }
                }
            }
            add_instrumented_hits(&mut report, &index, &aggregated_hits)?;
        } else {
            // Add hit data to the coverage report
            for suite in outcome.results.values() {
                for result in suite.test_results.values() {
                    let Some(hit_maps) = result.line_coverage.as_ref() else { continue };

                    for (code_hash, map) in &hit_maps.0 {
                        if let Some(resolved) = resolved_hit_maps.get(code_hash) {
                            report.add_hit_map(
                                &resolved.contract_id,
                                map,
                                resolved.is_deployed_code,
                            )?;
                            continue;
                        }

                        let Some((artifact_id, is_deployed_code)) = known_contracts
                            .find_by_deployed_code(map.bytecode())
                            .map(|(id, _)| (id, true))
                            .or_else(|| {
                                known_contracts
                                    .find_by_creation_code(map.bytecode())
                                    .map(|(id, _)| (id, false))
                            })
                        else {
                            continue;
                        };

                        let Some(source_id) =
                            report.get_source_id(&artifact_id.build_id, &artifact_id.source)
                        else {
                            continue;
                        };
                        let contract_id = ContractId {
                            version: artifact_id.version.clone(),
                            build_id: artifact_id.build_id.clone(),
                            source_id,
                            contract_name: artifact_id.name.as_str().into(),
                        };

                        report.add_hit_map(&contract_id, map, is_deployed_code)?;

                        resolved_hit_maps
                            .entry(*code_hash)
                            .or_insert(ResolvedHitMap { contract_id, is_deployed_code });
                    }
                }
            }
        }

        // Filter out ignored sources from the report.
        let file_root = filter.paths().root.as_path();
        if let Some(not_re) = &filter.args().coverage_pattern_inverse {
            report.retain_sources(|path: &Path| {
                let path = path.strip_prefix(file_root).unwrap_or(path);
                !not_re.is_match(&path.to_string_lossy())
            });
        }
        if !self.skip_files.is_empty() {
            let mut builder = GlobSetBuilder::new();
            for pattern in &self.skip_files {
                let glob = Glob::new(pattern).map_err(|e| {
                    eyre::eyre!("invalid glob in coverage.skip_files: '{pattern}': {e}")
                })?;
                builder.add(glob);
            }
            let set = builder
                .build()
                .map_err(|e| eyre::eyre!("failed to build coverage.skip_files glob set: {e}"))?;
            report.retain_sources(|path: &Path| {
                let path = path.strip_prefix(file_root).unwrap_or(path);
                !set.is_match(path)
            });
        }

        // Output final reports.
        self.report(&report)?;

        if self.report.iter().any(|kind| matches!(kind, CoverageReportKind::Attribution)) {
            let reporter = CoverageAttributionReporter::new(
                self.report_path(project_root, "coverage-attribution.json"),
            );
            reporter.report(&report, &outcome, &resolved_hit_maps)?;
        }

        // Check for test failures after generating coverage report.
        // This ensures coverage data is written even when tests fail.
        outcome.ensure_ok(false)?;

        Ok(())
    }

    #[instrument(name = "Coverage::report", skip_all)]
    fn report(&mut self, report: &CoverageReport) -> Result<()> {
        for reporter in &mut self.reporters {
            let _guard = debug_span!("reporter.report", kind=%reporter.name()).entered();
            reporter.report(report)?;
        }
        Ok(())
    }

    pub const fn is_watch(&self) -> bool {
        self.test.is_watch()
    }

    pub const fn watch(&self) -> &WatchArgs {
        &self.test.watch
    }
}

struct CoverageBuild {
    project: Project,
    output: ProjectCompileOutput,
    instrumentation: Option<InstrumentedCoverageMetadata>,
}

#[derive(Clone, Debug)]
struct InstrumentedCoverageItemIds {
    version: Version,
    line_item_id: u32,
    statement_item_id: Option<u32>,
    branch_item_id: Option<u32>,
    require_branch: Option<InstrumentedRequireBranchIds>,
    function_item_id: Option<u32>,
}

#[derive(Clone, Debug)]
struct InstrumentedRequireBranchIds {
    role: InstrumentedRequireProbeRole,
    false_item_id: u32,
    true_item_id: u32,
}

#[derive(Clone, Copy, Debug)]
enum InstrumentedRequireProbeRole {
    Pre,
    Post,
}

#[derive(Clone, Debug, Default)]
struct InstrumentedRequireBranchHits {
    pre: u32,
    post: u32,
    false_item_id: u32,
    true_item_id: u32,
}

#[derive(Clone, Debug, Default)]
struct InstrumentedCoverageTagIndex(B256HashMap<InstrumentedCoverageItemIds>);

/// Builds a [`CoverageItem`] of `kind` located at `probe`'s source range.
fn make_item(
    source_id: u32,
    probe: &InstrumentedCoverageProbe,
    kind: CoverageItemKind,
) -> CoverageItem {
    CoverageItem {
        kind,
        loc: SourceLocation {
            source_id: source_id as usize,
            contract_name: probe.contract_name.clone().into(),
            bytes: probe.bytes.clone(),
            lines: probe.lines.clone(),
        },
        hits: 0,
    }
}

/// Converts the probes of a single source into deduplicated [`CoverageItem`]s.
///
/// A line item is created once per source line; statements and functions once per source range;
/// branches once per `(branch_id, path_id)`. A `require` contributes a statement (from its pre
/// probe) plus the two branch paths it guards.
fn build_source_items(source_id: u32, probes: &[&InstrumentedCoverageProbe]) -> Vec<CoverageItem> {
    let mut items = Vec::new();
    let mut line_ranges = BTreeSet::new();
    let mut statement_ranges = BTreeSet::new();
    let mut branch_paths = BTreeSet::new();
    let mut function_ranges = BTreeSet::new();

    for probe in probes {
        if line_ranges.insert((probe.lines.start, probe.lines.end)) {
            items.push(make_item(source_id, probe, CoverageItemKind::Line));
        }
        match &probe.kind {
            InstrumentedCoverageProbeKind::Statement => {
                if statement_ranges.insert((probe.bytes.start, probe.bytes.end)) {
                    items.push(make_item(source_id, probe, CoverageItemKind::Statement));
                }
            }
            InstrumentedCoverageProbeKind::Branch { branch_id, path_id } => {
                if branch_paths.insert((*branch_id, *path_id)) {
                    items.push(make_item(
                        source_id,
                        probe,
                        CoverageItemKind::Branch {
                            branch_id: *branch_id,
                            path_id: *path_id,
                            is_first_opcode: true,
                        },
                    ));
                }
            }
            InstrumentedCoverageProbeKind::RequirePre { branch_id }
            | InstrumentedCoverageProbeKind::RequirePost { branch_id } => {
                // Only the pre probe carries the statement; both pre and post map to the same
                // two branch paths (false = 0, true = 1).
                if matches!(probe.kind, InstrumentedCoverageProbeKind::RequirePre { .. })
                    && statement_ranges.insert((probe.bytes.start, probe.bytes.end))
                {
                    items.push(make_item(source_id, probe, CoverageItemKind::Statement));
                }
                for path_id in [0, 1] {
                    if branch_paths.insert((*branch_id, path_id)) {
                        items.push(make_item(
                            source_id,
                            probe,
                            CoverageItemKind::Branch {
                                branch_id: *branch_id,
                                path_id,
                                is_first_opcode: false,
                            },
                        ));
                    }
                }
            }
            InstrumentedCoverageProbeKind::Function { name } => {
                if function_ranges.insert((probe.bytes.start, probe.bytes.end)) {
                    items.push(make_item(
                        source_id,
                        probe,
                        CoverageItemKind::Function { name: name.clone() },
                    ));
                }
            }
        }
    }

    items.sort();
    items
}

fn add_instrumented_hits(
    report: &mut CoverageReport,
    index: &InstrumentedCoverageTagIndex,
    hits: &InstrumentedHitMaps,
) -> Result<()> {
    let mut require_hits = BTreeMap::<(Version, u32, u32), InstrumentedRequireBranchHits>::new();

    for (tag, hit_count) in &hits.0 {
        let Some(item_ids) = index.0.get(tag) else { continue };
        let Some(analysis) = report.analyses.get_mut(&item_ids.version) else { continue };
        let items = analysis.all_items_mut();
        let hit_count = (*hit_count).min(u32::MAX as u64) as u32;

        if let Some(statement_item_id) = item_ids.statement_item_id
            && let Some(statement) = items.get_mut(statement_item_id as usize)
        {
            statement.hits = statement.hits.saturating_add(hit_count);
        }
        if let Some(branch_item_id) = item_ids.branch_item_id
            && let Some(branch) = items.get_mut(branch_item_id as usize)
        {
            branch.hits = branch.hits.saturating_add(hit_count);
        }
        if let Some(require_branch) = &item_ids.require_branch {
            let entry = require_hits
                .entry((
                    item_ids.version.clone(),
                    require_branch.false_item_id,
                    require_branch.true_item_id,
                ))
                .or_insert_with(|| InstrumentedRequireBranchHits {
                    false_item_id: require_branch.false_item_id,
                    true_item_id: require_branch.true_item_id,
                    ..Default::default()
                });
            match require_branch.role {
                InstrumentedRequireProbeRole::Pre => {
                    entry.pre = entry.pre.saturating_add(hit_count);
                }
                InstrumentedRequireProbeRole::Post => {
                    entry.post = entry.post.saturating_add(hit_count);
                }
            }
        }
        if let Some(function_item_id) = item_ids.function_item_id
            && let Some(function) = items.get_mut(function_item_id as usize)
        {
            function.hits = function.hits.max(hit_count);
        }
        if let Some(line) = items.get_mut(item_ids.line_item_id as usize) {
            line.hits = line.hits.max(hit_count);
        }
    }

    for ((version, _, _), hits) in require_hits {
        let Some(analysis) = report.analyses.get_mut(&version) else { continue };
        let items = analysis.all_items_mut();
        if let Some(branch) = items.get_mut(hits.false_item_id as usize) {
            branch.hits = branch.hits.saturating_add(hits.pre.saturating_sub(hits.post));
        }
        if let Some(branch) = items.get_mut(hits.true_item_id as usize) {
            branch.hits = branch.hits.saturating_add(hits.post);
        }
    }

    Ok(())
}
/// Helper function that will link references in unlinked bytecode to the 0 address.
///
/// This is needed in order to analyze the bytecode for contracts that use libraries.
fn dummy_link_bytecode(mut obj: CompactBytecode) -> Option<Bytes> {
    let link_references = obj.link_references.clone();
    for (file, libraries) in link_references {
        for library in libraries.keys() {
            obj.link(&file, library, Address::ZERO);
        }
    }

    obj.object.resolve();
    obj.object.into_bytes()
}

/// Helper function that will link references in unlinked bytecode to the 0 address.
///
/// This is needed in order to analyze the bytecode for contracts that use libraries.
fn dummy_link_deployed_bytecode(obj: CompactDeployedBytecode) -> Option<Bytes> {
    obj.bytecode.and_then(dummy_link_bytecode)
}

pub struct ArtifactData {
    pub contract_id: ContractId,
    pub creation: BytecodeData,
    pub deployed: BytecodeData,
    pub function_selectors: Vec<[u8; 4]>,
    pub has_receive: bool,
    pub fallback_payable: bool,
}

impl ArtifactData {
    pub fn new(id: &ArtifactId, source_id: usize, artifact: &impl Artifact) -> Option<Self> {
        let abi = artifact.get_abi();
        let function_selectors = abi
            .as_ref()
            .map(|abi| abi.functions().map(|function| function.selector().into()).collect())
            .unwrap_or_default();
        let has_receive = abi.as_ref().is_some_and(|abi| abi.receive.is_some());
        let fallback_payable = abi
            .as_ref()
            .and_then(|abi| abi.fallback)
            .is_some_and(|fallback| fallback.state_mutability == StateMutability::Payable);
        Some(Self {
            contract_id: ContractId {
                version: id.version.clone(),
                build_id: id.build_id.clone(),
                source_id,
                contract_name: id.name.as_str().into(),
            },
            creation: BytecodeData::new(
                artifact.get_source_map()?.ok()?,
                artifact
                    .get_bytecode()
                    .and_then(|bytecode| dummy_link_bytecode(bytecode.into_owned()))?,
            ),
            deployed: BytecodeData::new(
                artifact.get_source_map_deployed()?.ok()?,
                artifact
                    .get_deployed_bytecode()
                    .and_then(|bytecode| dummy_link_deployed_bytecode(bytecode.into_owned()))?,
            ),
            function_selectors,
            has_receive,
            fallback_payable,
        })
    }
}

pub struct BytecodeData {
    source_map: SourceMap,
    bytecode: Bytes,
    /// The instruction counter to program counter mapping.
    ///
    /// The source maps are indexed by *instruction counters*, which are the indexes of
    /// instructions in the bytecode *minus any push bytes*.
    ///
    /// Since our line coverage inspector collects hit data using program counters, the anchors
    /// also need to be based on program counters.
    ic_pc_map: IcPcMap,
}

impl BytecodeData {
    fn new(source_map: SourceMap, bytecode: Bytes) -> Self {
        let ic_pc_map = IcPcMap::new(&bytecode);
        Self { source_map, bytecode, ic_pc_map }
    }

    pub fn find_anchors(&self, source_analysis: &SourceAnalysis) -> Vec<ItemAnchor> {
        find_anchors(&self.bytecode, &self.source_map, &self.ic_pc_map, source_analysis)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lcov_version() {
        assert_eq!(parse_lcov_version("0").unwrap(), Version::new(0, 0, 0));
        assert_eq!(parse_lcov_version("1").unwrap(), Version::new(1, 0, 0));
        assert_eq!(parse_lcov_version("1.0").unwrap(), Version::new(1, 0, 0));
        assert_eq!(parse_lcov_version("1.1").unwrap(), Version::new(1, 1, 0));
        assert_eq!(parse_lcov_version("1.11").unwrap(), Version::new(1, 11, 0));
    }

    #[test]
    fn resolve_lcov_version_uses_config_when_cli_absent() {
        let mut args = CoverageArgs::parse_from(["coverage"]);
        let config = CoverageConfig { lcov_version: Version::new(2, 2, 0), ..Default::default() };

        args.resolve_with(&config);

        assert_eq!(args.lcov_version, Version::new(2, 2, 0));
    }

    #[test]
    fn resolve_lcov_version_keeps_explicit_cli_default() {
        let mut args = CoverageArgs::parse_from(["coverage", "--lcov-version", "1"]);
        let config = CoverageConfig { lcov_version: Version::new(2, 2, 0), ..Default::default() };

        args.resolve_with(&config);

        assert_eq!(args.lcov_version, Version::new(1, 0, 0));
    }

    #[test]
    fn resolve_lcov_version_keeps_explicit_cli_value() {
        let mut args = CoverageArgs::parse_from(["coverage", "--lcov-version", "2"]);
        let config = CoverageConfig { lcov_version: Version::new(2, 2, 0), ..Default::default() };

        args.resolve_with(&config);

        assert_eq!(args.lcov_version, Version::new(2, 0, 0));
    }
}
