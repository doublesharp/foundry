use super::{
    CoverageItem, CoverageItemKind, CoverageReport, InstrumentedHitMaps, SourceLocation,
    analysis::SourceAnalysis,
    instrumentation::{
        InstrumentedCoverageMetadata, InstrumentedCoverageProbe, InstrumentedCoverageProbeKind,
    },
};
use alloy_primitives::map::{B256HashMap, HashMap};
use eyre::Result;
use foundry_compilers::{ProjectCompileOutput, ProjectPathsConfig, VYPER_EXTENSIONS};
use semver::Version;
use std::collections::{BTreeMap, BTreeSet};

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
pub(crate) struct InstrumentedCoverageTagIndex(B256HashMap<InstrumentedCoverageItemIds>);

pub(crate) fn prepare(
    project_paths: &ProjectPathsConfig,
    output: &ProjectCompileOutput,
    instrumentation: &InstrumentedCoverageMetadata,
    include_libs: bool,
    exclude_tests: bool,
) -> Result<(CoverageReport, InstrumentedCoverageTagIndex)> {
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

        if (!include_libs && project_paths.has_library_ancestor(path))
            || (exclude_tests && project_paths.is_test(path))
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
        if included_sources.contains(&(probe.version.clone(), probe.path.clone(), source_id as u32))
        {
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
                let line_item_id =
                    line_item_ids.get(&(*source_id, probe.lines.start, probe.lines.end)).copied();
                if let Some(line_item_id) = line_item_id {
                    let statement_item_id = match probe.kind {
                        InstrumentedCoverageProbeKind::Statement
                        | InstrumentedCoverageProbeKind::RequirePre { .. } => statement_item_ids
                            .get(&(*source_id, probe.bytes.start, probe.bytes.end))
                            .copied(),
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
                            false_item_id.zip(true_item_id).map(|(false_item_id, true_item_id)| {
                                InstrumentedRequireBranchIds {
                                    role: InstrumentedRequireProbeRole::Pre,
                                    false_item_id,
                                    true_item_id,
                                }
                            })
                        }
                        InstrumentedCoverageProbeKind::RequirePost { branch_id } => {
                            let false_item_id =
                                branch_item_ids.get(&(*source_id, branch_id, 0)).copied();
                            let true_item_id =
                                branch_item_ids.get(&(*source_id, branch_id, 1)).copied();
                            false_item_id.zip(true_item_id).map(|(false_item_id, true_item_id)| {
                                InstrumentedRequireBranchIds {
                                    role: InstrumentedRequireProbeRole::Post,
                                    false_item_id,
                                    true_item_id,
                                }
                            })
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

    Ok((report, tag_index))
}

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

pub(crate) fn add_hits(
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
