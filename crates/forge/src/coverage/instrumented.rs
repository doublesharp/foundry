//! Converts source instrumentation metadata and runtime hits into coverage items.

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
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug)]
struct InstrumentedCoverageItemIds {
    build_id: String,
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
    let mut probes_by_source = HashMap::<_, Vec<_>>::default();
    for probe in &instrumentation.probes {
        probes_by_source.entry((&probe.version, probe.path.as_path())).or_default().push(probe);
    }
    let mut grouped = HashMap::<&str, BTreeMap<u32, &[&InstrumentedCoverageProbe]>>::default();
    let mut seen_sources = BTreeSet::new();

    for (path, sources) in &output.output().sources.0 {
        if path
            .extension()
            .and_then(|s| s.to_str())
            .is_some_and(|ext| VYPER_EXTENSIONS.contains(&ext))
        {
            continue;
        }

        for source in sources {
            let source_file = &source.source_file;
            if !seen_sources.insert((&source.version, path, source_file.id)) {
                continue;
            }

            report.add_source(source.build_id.clone(), source_file.id as usize, path.clone());

            if (!include_libs && project_paths.has_library_ancestor(path))
                || (exclude_tests && project_paths.is_test(path))
            {
                continue;
            }

            if let Some(probes) = probes_by_source.get(&(&source.version, path.as_path())) {
                grouped.entry(&source.build_id).or_default().insert(source_file.id, probes);
            }
        }
    }

    let mut tag_index = InstrumentedCoverageTagIndex::default();
    for (build_id, sources) in grouped {
        let analysis = SourceAnalysis::from_sourced_items(
            sources
                .iter()
                .map(|(&source_id, probes)| (source_id, build_source_items(source_id, probes)))
                .collect(),
        );
        let mut line_item_ids = HashMap::<(u32, u32, u32), u32>::default();
        let mut statement_item_ids = HashMap::<(u32, u32, u32), u32>::default();
        let mut branch_item_ids = HashMap::<(u32, u32, u32), u32>::default();
        let mut function_item_ids = HashMap::<(u32, u32, u32), u32>::default();

        for (source_id, probes) in &sources {
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

            for probe in *probes {
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
                            build_id: build_id.to_owned(),
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
        report.add_analysis(build_id.to_owned(), analysis);
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
        anchor_loc: None,
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
) {
    for ((build_id, item_id), hits) in item_hits(index, std::iter::once(hits)) {
        if let Some(analysis) = report.analyses.get_mut(build_id)
            && let Some(item) = analysis.all_items_mut().get_mut(item_id as usize)
        {
            item.hits = item.hits.saturating_add(hits);
        }
    }
}

/// Resolves runtime tags to item hits for either a whole run or one test.
pub(crate) fn item_hits<'a, 'b>(
    index: &'a InstrumentedCoverageTagIndex,
    hits: impl IntoIterator<Item = &'b InstrumentedHitMaps, IntoIter: Clone>,
) -> BTreeMap<(&'a str, u32), u32> {
    let mut items = BTreeMap::<(&str, u32), u32>::new();
    let maps = hits.into_iter();
    let mut require_hits = BTreeMap::<(&str, u32, u32), InstrumentedRequireBranchHits>::new();

    for (map_index, map) in maps.clone().enumerate() {
        for (tag, hit_count) in &map.0 {
            if maps.clone().take(map_index).any(|previous| previous.0.contains_key(tag)) {
                continue;
            }
            let Some(item_ids) = index.0.get(tag) else { continue };
            let build_id = item_ids.build_id.as_str();
            // Merge each tag across shared setup and test maps before reducing line maxima or
            // require pre/post differences, without cloning the underlying hit maps.
            let hit_count = maps
                .clone()
                .skip(map_index + 1)
                .fold(*hit_count, |count, map| count + map.0.get(tag).copied().unwrap_or_default())
                .min(u32::MAX as u64) as u32;
            for item_id in
                [item_ids.statement_item_id, item_ids.branch_item_id].into_iter().flatten()
            {
                let count = items.entry((build_id, item_id)).or_default();
                *count = count.saturating_add(hit_count);
            }
            if let Some(require_branch) = &item_ids.require_branch {
                let entry = require_hits
                    .entry((build_id, require_branch.false_item_id, require_branch.true_item_id))
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
            for item_id in
                [item_ids.function_item_id, Some(item_ids.line_item_id)].into_iter().flatten()
            {
                let count = items.entry((build_id, item_id)).or_default();
                *count = (*count).max(hit_count);
            }
        }
    }

    for ((build_id, _, _), hits) in require_hits {
        items.insert((build_id, hits.false_item_id), hits.pre.saturating_sub(hits.post));
        items.insert((build_id, hits.true_item_id), hits.post);
    }
    items.retain(|_, hits| *hits != 0);
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        coverage::{AttributionIndex, ResolvedHitMaps, attributed_items},
        result::TestResult,
    };
    use alloy_primitives::B256;
    use foundry_compilers::{Project, artifacts::SourceFile, output::sources::VersionedSourceFile};
    use semver::Version;
    use std::{path::Path, sync::Arc};

    fn probe(version: u64, path: &str, tag: u8, line: u32) -> InstrumentedCoverageProbe {
        InstrumentedCoverageProbe {
            version: Version::new(0, 8, version),
            path: path.into(),
            contract_name: "Counter".into(),
            tag: B256::repeat_byte(tag),
            kind: InstrumentedCoverageProbeKind::Statement,
            bytes: line * 10..line * 10 + 5,
            lines: line..line + 1,
        }
    }

    #[test]
    fn prepare_preserves_sources_versions_and_probe_order() {
        let root = tempfile::tempdir().unwrap();
        let mut output = Project::builder()
            .paths(ProjectPathsConfig::builder().build_with_root(root.path()))
            .ephemeral()
            .no_artifacts()
            .build(Default::default())
            .unwrap()
            .compile()
            .unwrap();
        for (path, version, build_id, source_id) in [
            ("src/Counter.sol", 20, "older", 4),
            ("src/Counter.sol", 21, "newer", 1),
            ("src/Other.sol", 21, "newer", 2),
            ("src/Counter.sol", 21, "duplicate", 1),
            ("src/Empty.sol", 21, "newer", 3),
        ] {
            output.output_mut().sources.0.entry(path.into()).or_default().push(
                VersionedSourceFile {
                    source_file: SourceFile { id: source_id, ast: None },
                    version: Version::new(0, 8, version),
                    build_id: build_id.into(),
                    profile: "default".into(),
                },
            );
        }
        let mut first = probe(21, "src/Counter.sol", 1, 2);
        first.contract_name = "First".into();
        let mut duplicate = first.clone();
        duplicate.contract_name = "Second".into();
        duplicate.tag = B256::repeat_byte(2);
        let metadata = InstrumentedCoverageMetadata {
            probes: vec![
                first,
                probe(20, "src/Counter.sol", 3, 4),
                probe(21, "src/Other.sol", 4, 3),
                duplicate,
                probe(22, "src/Counter.sol", 5, 5),
                probe(21, "src/Unknown.sol", 6, 6),
            ],
        };
        let paths = ProjectPathsConfig::builder().build_with_root(Path::new("."));
        let (report, index) = prepare(&paths, &output, &metadata, true, false).unwrap();
        assert_eq!(report.analyses.len(), 2);
        assert_eq!(report.source_paths["newer"].len(), 3);
        assert!(!report.source_paths.contains_key("duplicate"));
        let newer = report.analyses["newer"].all_items();
        assert_eq!(newer.len(), 4);
        assert_eq!(newer[0].loc.contract_name.as_ref(), "First");
        assert_eq!(newer[1].loc.contract_name.as_ref(), "First");
        assert_eq!(newer[2].loc.source_id, 2);
        assert_eq!(report.analyses["older"].all_items()[0].loc.lines, 4..5);
        assert_eq!(index.0.len(), 4);
        assert_eq!(index.0[&B256::repeat_byte(1)].statement_item_id, Some(1));
        assert_eq!(index.0[&B256::repeat_byte(2)].statement_item_id, Some(1));
        assert_eq!(index.0[&B256::repeat_byte(4)].statement_item_id, Some(3));
    }
    #[test]
    fn item_hits_resolves_combined_tags_before_lines_and_require_branches() {
        let mut index = InstrumentedCoverageTagIndex::default();
        for (tag, require_branch) in [
            (1, Some(InstrumentedRequireProbeRole::Pre)),
            (2, Some(InstrumentedRequireProbeRole::Post)),
            (3, None),
        ] {
            index.0.insert(
                B256::repeat_byte(tag),
                InstrumentedCoverageItemIds {
                    build_id: "build".into(),
                    line_item_id: 0,
                    statement_item_id: (tag == 3).then_some(1),
                    branch_item_id: None,
                    require_branch: require_branch.map(|role| InstrumentedRequireBranchIds {
                        role,
                        false_item_id: 2,
                        true_item_id: 3,
                    }),
                    function_item_id: None,
                },
            );
        }
        let setup = InstrumentedHitMaps(
            [(B256::repeat_byte(1), 2), (B256::repeat_byte(2), 1), (B256::repeat_byte(3), 4)]
                .into_iter()
                .collect(),
        );
        let delta = InstrumentedHitMaps(
            [(B256::repeat_byte(1), 3), (B256::repeat_byte(2), 4), (B256::repeat_byte(3), 2)]
                .into_iter()
                .collect(),
        );
        let mut merged = setup.clone();
        merged.merge_ref(&delta);
        assert_eq!(item_hits(&index, [&setup, &delta]), item_hits(&index, [&merged]));
        assert_eq!(
            item_hits(&index, [&merged]),
            BTreeMap::from([(("build", 0), 6), (("build", 1), 6), (("build", 3), 5)])
        );
        let mut report = CoverageReport::default();
        report.add_source("build".into(), 0, "src/Counter.sol".into());
        let items = [
            CoverageItemKind::Line,
            CoverageItemKind::Statement,
            CoverageItemKind::Branch { branch_id: 0, path_id: 0, is_first_opcode: false },
            CoverageItemKind::Branch { branch_id: 0, path_id: 1, is_first_opcode: false },
        ]
        .into_iter()
        .map(|kind| CoverageItem {
            kind,
            loc: SourceLocation {
                source_id: 0,
                contract_name: "Counter".into(),
                bytes: 10..15,
                lines: 2..3,
            },
            anchor_loc: None,
            hits: 0,
        })
        .collect();
        report.add_analysis("build".into(), SourceAnalysis::from_sourced_items(vec![(0, items)]));
        let result = TestResult {
            setup_instrumented_coverage: Some(Arc::new(setup)),
            instrumented_coverage: Some(delta),
            ..Default::default()
        };
        let paths = AttributionIndex::source_paths(&report);
        let metadata = AttributionIndex::new(&report, &paths);
        let attributed =
            attributed_items(&metadata, &ResolvedHitMaps::default(), Some(&index), &result);
        assert_eq!(
            attributed.iter().map(|item| (item.kind, item.path_id, item.hits)).collect::<Vec<_>>(),
            vec![("branch", Some(1), 5), ("line", None, 6), ("statement", None, 6)]
        );
    }
}
