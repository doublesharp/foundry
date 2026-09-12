//! Coverage reports.

use crate::result::{TestKind, TestOutcome, TestResult, TestStatus};
use alloy_primitives::map::{HashMap, HashSet};
use comfy_table::{
    Attribute, Cell, Color, Row, Table,
    presets::{ASCII_FULL, ASCII_MARKDOWN},
};
use evm_disassembler::disassemble_bytes;
use foundry_common::{fs, shell};
use semver::Version;
use serde::{Serialize, ser::SerializeSeq};
use std::{
    collections::{BTreeMap, hash_map},
    io::Write,
    path::{Path, PathBuf},
};

pub use foundry_evm::coverage::*;

pub(crate) mod instrumented;

/// A coverage reporter.
pub trait CoverageReporter {
    /// Returns a debug string for the reporter.
    fn name(&self) -> &'static str;

    /// Returns `true` if the reporter needs source maps for the final report.
    fn needs_source_maps(&self) -> bool {
        false
    }

    /// Runs the reporter.
    fn report(&mut self, report: &CoverageReport) -> eyre::Result<()>;
}

/// A simple summary reporter that prints the coverage results in a table.
pub struct CoverageSummaryReporter {
    /// The summary table.
    table: Table,
    /// The total coverage of the entire project.
    total: CoverageSummary,
}

impl Default for CoverageSummaryReporter {
    fn default() -> Self {
        let mut table = Table::new();
        if shell::is_markdown() {
            table.load_style(ASCII_MARKDOWN);
        } else {
            table.load_style(ASCII_FULL.with_rounded_corners());
        }

        table.set_header(vec![
            Cell::new("File"),
            Cell::new("% Lines"),
            Cell::new("% Statements"),
            Cell::new("% Branches"),
            Cell::new("% Funcs"),
        ]);

        Self { table, total: CoverageSummary::default() }
    }
}

impl CoverageSummaryReporter {
    fn add_row(&mut self, name: impl Into<Cell>, summary: CoverageSummary) {
        let mut row = Row::new();
        row.add_cell(name.into())
            .add_cell(format_cell(summary.line_hits, summary.line_count))
            .add_cell(format_cell(summary.statement_hits, summary.statement_count))
            .add_cell(format_cell(summary.branch_hits, summary.branch_count))
            .add_cell(format_cell(summary.function_hits, summary.function_count));
        self.table.add_row(row);
    }
}

impl CoverageReporter for CoverageSummaryReporter {
    fn name(&self) -> &'static str {
        "summary"
    }

    fn report(&mut self, report: &CoverageReport) -> eyre::Result<()> {
        for (path, summary) in report.summary_by_file() {
            self.total.merge(&summary);
            self.add_row(path.display(), summary);
        }

        self.add_row("Total", self.total.clone());
        sh_println!("\n{}", self.table)?;
        Ok(())
    }
}

fn format_cell(hits: usize, total: usize) -> Cell {
    if total == 0 {
        return Cell::new(format!("N/A ({hits}/{total})"))
            .fg(Color::Grey)
            .add_attribute(Attribute::Dim);
    }

    let percentage = hits as f64 / total as f64;
    Cell::new(format!("{:.2}% ({hits}/{total})", percentage * 100.)).fg(match percentage {
        _ if percentage < 0.5 => Color::Red,
        _ if percentage < 0.75 => Color::Yellow,
        _ => Color::Green,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use alloy_primitives::{B256, Bytes};
    use foundry_evm::coverage::analysis::SourceAnalysis;

    #[test]
    fn attribution_merges_builds_preserving_metadata_order_and_saturation() {
        let mut report = CoverageReport::default();
        let mut resolved = ResolvedHitMaps::default();
        let mut hit_maps = HitMaps::default();
        for (build_id, source_id, tag, count) in [("older", 4, 1, u32::MAX), ("newer", 1, 2, 3)] {
            report.add_source(build_id.into(), source_id, "src/Counter.sol".into());
            let kinds = [
                CoverageItemKind::Statement,
                CoverageItemKind::Function { name: "z()".into() },
                CoverageItemKind::Branch { branch_id: 7, path_id: 1, is_first_opcode: true },
                CoverageItemKind::Branch { branch_id: 7, path_id: 0, is_first_opcode: false },
                CoverageItemKind::Function { name: "a()".into() },
            ];
            let items = kinds
                .into_iter()
                .map(|kind| CoverageItem {
                    kind,
                    loc: SourceLocation {
                        source_id,
                        contract_name: "Counter".into(),
                        bytes: 10..15,
                        lines: 2..3,
                    },
                    anchor_loc: None,
                    hits: 0,
                })
                .collect();
            report.add_analysis(
                build_id.into(),
                SourceAnalysis::from_sourced_items(vec![(source_id as u32, items)]),
            );
            let contract_id = ContractId {
                version: Version::new(0, 8, 20 + tag),
                build_id: build_id.into(),
                source_id,
                contract_name: "Counter".into(),
            };
            report.add_anchors([(
                contract_id.clone(),
                (
                    (0..5).map(|item_id| ItemAnchor { instruction: item_id, item_id }).collect(),
                    Vec::new(),
                ),
            )]);
            let mut hits = HitMap::new(Bytes::new());
            for instruction in 0..5 {
                hits.hits(instruction, count);
            }
            let hash = B256::repeat_byte(tag as u8);
            hit_maps.0.insert(hash, hits);
            resolved.insert(hash, ResolvedHitMap { contract_id, is_deployed_code: false });
        }
        let result = TestResult { line_coverage: Some(hit_maps), ..Default::default() };
        let source_paths = AttributionIndex::source_paths(&report);
        let metadata = AttributionIndex::new(&report, &source_paths);
        assert_eq!(source_paths.len(), 1);
        assert_eq!(metadata.items.len(), 5);
        let attributed = attributed_items(&metadata, &resolved, None, &result);
        let actual = serde_json::to_value(&attributed).unwrap();
        let common = serde_json::json!({"source":"src/Counter.sol","contract":"Counter","kind":"statement","line_start":2,"line_end":3,"byte_start":10,"byte_end":15,"hits":u32::MAX});
        let mut expected = Vec::new();
        for (kind, function, path_id) in [
            ("branch", None, Some(0)),
            ("branch", None, Some(1)),
            ("function", Some("a()"), None),
            ("function", Some("z()"), None),
            ("statement", None, None),
        ] {
            let mut item = common.clone();
            item["kind"] = kind.into();
            if let Some(name) = function {
                item["function"] = name.into();
            }
            if let Some(path_id) = path_id {
                item["branch_id"] = 7.into();
                item["path_id"] = path_id.into();
            }
            expected.push(item);
        }
        assert_eq!(actual, serde_json::json!(expected));
        for item in &attributed {
            assert!(
                report.analyses.values().flat_map(|analysis| analysis.all_items()).any(
                    |original| { original.loc.contract_name.as_ptr() == item.contract.as_ptr() }
                ),
                "attribution must borrow canonical contract metadata"
            );
            if let Some(function) = &item.function {
                assert!(report.analyses.values().flat_map(|analysis| analysis.all_items()).any(|original| {
                    matches!(&original.kind, CoverageItemKind::Function { name } if name.as_ptr() == function.as_ptr())
                }), "attribution must borrow canonical function metadata");
            }
        }
    }

    #[test]
    fn empty_summary_cell_is_not_applicable() {
        assert_eq!(
            format_cell(0, 0),
            Cell::new("N/A (0/0)").fg(Color::Grey).add_attribute(Attribute::Dim)
        );
    }
}

/// Writes the coverage report in [LCOV]'s [tracefile format].
///
/// [LCOV]: https://github.com/linux-test-project/lcov
/// [tracefile format]: https://man.archlinux.org/man/geninfo.1.en#TRACEFILE_FORMAT
pub struct LcovReporter {
    path: PathBuf,
    version: Version,
}

impl LcovReporter {
    /// Create a new LCOV reporter.
    pub const fn new(path: PathBuf, version: Version) -> Self {
        Self { path, version }
    }
}

impl CoverageReporter for LcovReporter {
    fn name(&self) -> &'static str {
        "lcov"
    }

    fn report(&mut self, report: &CoverageReport) -> eyre::Result<()> {
        let mut out = std::io::BufWriter::new(fs::create_file(&self.path)?);

        let mut fn_index = 0usize;
        for (path, items) in report.items_by_file() {
            let summary = CoverageSummary::from_items(&items);

            writeln!(out, "TN:")?;
            writeln!(out, "SF:{}", path.display())?;

            // First pass: collect line hits for DA records.
            // Track both which lines have been recorded and the max hits per line.
            let mut line_hits: HashMap<u32, u32> = HashMap::default();
            for item in &items {
                if matches!(item.kind, CoverageItemKind::Line | CoverageItemKind::Statement) {
                    let line = item.loc.lines.start;
                    line_hits
                        .entry(line)
                        .and_modify(|h| *h = (*h).max(item.hits))
                        .or_insert(item.hits);
                }
            }

            let mut recorded_lines = HashSet::new();

            for item in items {
                let line = item.loc.lines.start;
                // `lines` is half-open, so we need to subtract 1 to get the last included line.
                let end_line = item.loc.lines.end - 1;
                let hits = item.hits;
                match item.kind {
                    CoverageItemKind::Function { ref name } => {
                        // Free (file-level) functions have no contract scope; emit the bare
                        // name rather than a leading-dot `.name`.
                        let name = if item.loc.contract_name.is_empty() {
                            name.to_string()
                        } else {
                            format!("{}.{name}", item.loc.contract_name)
                        };
                        if self.version >= Version::new(2, 2, 0) {
                            // v2.2 changed the FN format.
                            writeln!(out, "FNL:{fn_index},{line},{end_line}")?;
                            writeln!(out, "FNA:{fn_index},{hits},{name}")?;
                            fn_index += 1;
                        } else if self.version >= Version::new(2, 0, 0) {
                            // v2.0 added end_line to FN.
                            writeln!(out, "FN:{line},{end_line},{name}")?;
                            writeln!(out, "FNDA:{hits},{name}")?;
                        } else {
                            writeln!(out, "FN:{line},{name}")?;
                            writeln!(out, "FNDA:{hits},{name}")?;
                        }
                    }
                    // Add lines / statement hits only once.
                    CoverageItemKind::Line | CoverageItemKind::Statement
                        if recorded_lines.insert(line) =>
                    {
                        writeln!(out, "DA:{line},{hits}")?;
                    }
                    CoverageItemKind::Branch { branch_id, path_id, .. } => {
                        // Per LCOV spec: "-" means the expression was never evaluated (line not
                        // executed), "0" means branch exists but was never taken.
                        // Check if the line containing this branch was hit.
                        let line_was_hit = line_hits.get(&line).is_some_and(|&h| h > 0);
                        let hits_str = if hits > 0 {
                            hits.to_string()
                        } else if line_was_hit {
                            "0".to_string()
                        } else {
                            "-".to_string()
                        };
                        writeln!(out, "BRDA:{line},{branch_id},{path_id},{hits_str}")?;
                    }
                    _ => {}
                }
            }

            // Function summary
            writeln!(out, "FNF:{}", summary.function_count)?;
            writeln!(out, "FNH:{}", summary.function_hits)?;

            // Line summary
            writeln!(out, "LF:{}", summary.line_count)?;
            writeln!(out, "LH:{}", summary.line_hits)?;

            // Branch summary
            writeln!(out, "BRF:{}", summary.branch_count)?;
            writeln!(out, "BRH:{}", summary.branch_hits)?;

            writeln!(out, "end_of_record")?;
        }

        out.flush()?;
        sh_println!("Wrote LCOV report.")?;

        Ok(())
    }
}

/// Writes per-test coverage attribution as JSON.
pub struct CoverageAttributionReporter {
    path: PathBuf,
}

/// A hit map resolved to the contract coverage metadata it belongs to.
pub struct ResolvedHitMap {
    pub contract_id: ContractId,
    pub is_deployed_code: bool,
}

pub type ResolvedHitMaps = alloy_primitives::map::B256HashMap<ResolvedHitMap>;

impl CoverageAttributionReporter {
    /// Create a new attribution reporter.
    pub const fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Writes per-test coverage attribution for the provided outcome.
    pub(crate) fn report(
        &self,
        report: &CoverageReport,
        outcome: &TestOutcome,
        resolved_hit_maps: &ResolvedHitMaps,
        instrumented_index: Option<&instrumented::InstrumentedCoverageTagIndex>,
    ) -> eyre::Result<()> {
        let payload = AttributionReport {
            version: 1,
            tests: AttributionTests { report, outcome, resolved_hit_maps, instrumented_index },
        };
        let mut out = std::io::BufWriter::new(fs::create_file(&self.path)?);
        serde_json::to_writer(&mut out, &payload)?;
        writeln!(out)?;
        out.flush()?;

        sh_println!("Wrote coverage attribution report.")?;

        Ok(())
    }
}

/// Writes the coverage report in Istanbul's JSON coverage-map format.
pub struct JsonReporter {
    root: PathBuf,
    path: PathBuf,
}

impl JsonReporter {
    /// Create a new JSON reporter.
    pub const fn new(root: PathBuf, path: PathBuf) -> Self {
        Self { root, path }
    }
}

impl CoverageReporter for JsonReporter {
    fn name(&self) -> &'static str {
        "json"
    }

    fn report(&mut self, report: &CoverageReport) -> eyre::Result<()> {
        let mut coverage = BTreeMap::new();
        let mut locations = SourcePositionCache::new(self.root.clone());

        for (path, items) in report.items_by_file() {
            let mut file = IstanbulFileCoverage {
                path: path.display().to_string(),
                statement_map: BTreeMap::new(),
                function_map: BTreeMap::new(),
                branch_map: BTreeMap::new(),
                statements: BTreeMap::new(),
                functions: BTreeMap::new(),
                branches: BTreeMap::new(),
            };

            let mut statement_index = 0usize;
            let mut function_index = 0usize;
            let mut branch_groups = BTreeMap::<u32, Vec<&CoverageItem>>::new();

            for item in &items {
                match &item.kind {
                    CoverageItemKind::Statement => {
                        let id = statement_index.to_string();
                        statement_index += 1;
                        file.statement_map.insert(id.clone(), locations.location(path, &item.loc)?);
                        file.statements.insert(id, item.hits);
                    }
                    CoverageItemKind::Function { name } => {
                        let id = function_index.to_string();
                        function_index += 1;
                        let loc = locations.location(path, &item.loc)?;
                        file.function_map.insert(
                            id.clone(),
                            IstanbulFunction {
                                name: name.to_string(),
                                decl: loc.clone(),
                                loc,
                                line: item.loc.lines.start,
                            },
                        );
                        file.functions.insert(id, item.hits);
                    }
                    CoverageItemKind::Branch { branch_id, .. } => {
                        branch_groups.entry(*branch_id).or_default().push(item);
                    }
                    CoverageItemKind::Line => {}
                }
            }

            for (branch_index, branch_items) in branch_groups.values_mut().enumerate() {
                branch_items.sort_by_key(|item| match item.kind {
                    CoverageItemKind::Branch { path_id, .. } => path_id,
                    _ => unreachable!(),
                });

                let id = branch_index.to_string();
                let loc = locations.location(path, &branch_items[0].loc)?;
                let mut branch_locations = Vec::with_capacity(branch_items.len());
                let mut hits = Vec::with_capacity(branch_items.len());
                for item in branch_items {
                    branch_locations.push(locations.location(path, &item.loc)?);
                    hits.push(item.hits);
                }
                file.branch_map.insert(
                    id.clone(),
                    IstanbulBranch { loc, branch_type: "branch", locations: branch_locations },
                );
                file.branches.insert(id, hits);
            }

            coverage.insert(path.display().to_string(), file);
        }

        serde_json::to_writer_pretty(fs::create_file(&self.path)?, &coverage)?;
        sh_println!("Wrote JSON coverage report.")?;

        Ok(())
    }
}

/// Top-level JSON payload for per-test coverage attribution.
#[derive(Serialize)]
struct AttributionReport<'a> {
    version: u8,
    tests: AttributionTests<'a>,
}

/// Coverage attributed to a single executed test.
#[derive(Serialize)]
struct AttributionTest<'a> {
    suite: &'a str,
    test: &'a str,
    status: &'static str,
    kind: &'static str,
    covered: Vec<AttributionItem<'a>>,
}

/// A source range covered by a test, with hit counts and item metadata.
#[derive(Clone, Copy, Serialize)]
struct AttributionItem<'a> {
    source: &'a str,
    contract: &'a str,
    kind: &'static str,
    /// The start of a 1-based, half-open line range.
    line_start: u32,
    /// The end of a 1-based, half-open line range.
    line_end: u32,
    /// The start of a 0-based, half-open byte range.
    byte_start: u32,
    /// The end of a 0-based, half-open byte range.
    byte_end: u32,
    hits: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    function: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    branch_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path_id: Option<u32>,
}

/// Serializer state for streaming attribution entries from test results.
struct AttributionTests<'a> {
    report: &'a CoverageReport,
    outcome: &'a TestOutcome,
    resolved_hit_maps: &'a ResolvedHitMaps,
    instrumented_index: Option<&'a instrumented::InstrumentedCoverageTagIndex>,
}

impl Serialize for AttributionTests<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let len = self.outcome.results.values().map(|suite| suite.test_results.len()).sum();
        let mut seq = serializer.serialize_seq(Some(len))?;
        let source_paths = AttributionIndex::source_paths(self.report);
        let metadata = AttributionIndex::new(self.report, &source_paths);

        for (suite, suite_result) in &self.outcome.results {
            for (test, result) in &suite_result.test_results {
                seq.serialize_element(&AttributionTest {
                    suite,
                    test,
                    status: test_status_name(result.status),
                    kind: test_kind_name(&result.kind),
                    covered: attributed_items(
                        &metadata,
                        self.resolved_hit_maps,
                        self.instrumented_index,
                        result,
                    ),
                })?;
            }
        }

        seq.end()
    }
}

type AttributionItemKey<'a> =
    (&'a str, &'a str, &'static str, u32, u32, u32, u32, Option<&'a str>, Option<u32>, Option<u32>);

impl<'a> AttributionItem<'a> {
    fn new(source: &'a str, item: &'a CoverageItem) -> Self {
        let (kind, function, branch_id, path_id) = coverage_item_kind_fields(&item.kind);
        Self {
            source,
            contract: &item.loc.contract_name,
            kind,
            line_start: item.loc.lines.start,
            line_end: item.loc.lines.end,
            byte_start: item.loc.bytes.start,
            byte_end: item.loc.bytes.end,
            hits: 0,
            function,
            branch_id,
            path_id,
        }
    }

    const fn key(&self) -> AttributionItemKey<'a> {
        (
            self.source,
            self.contract,
            self.kind,
            self.line_start,
            self.line_end,
            self.byte_start,
            self.byte_end,
            self.function,
            self.branch_id,
            self.path_id,
        )
    }
}

/// Canonical metadata shared by every test, with IDs in the JSON item sort order.
struct AttributionIndex<'a> {
    report: &'a CoverageReport,
    source_paths: &'a HashMap<&'a Path, String>,
    items: Vec<AttributionItem<'a>>,
    item_ids: HashMap<AttributionItemKey<'a>, usize>,
}

impl<'a> AttributionIndex<'a> {
    fn source_paths(report: &CoverageReport) -> HashMap<&Path, String> {
        let mut paths = HashMap::default();
        for source_paths in report.source_paths.values() {
            for path in source_paths.values() {
                paths.entry(path.as_path()).or_insert_with(|| path.display().to_string());
            }
        }
        paths
    }

    fn new(report: &'a CoverageReport, source_paths: &'a HashMap<&'a Path, String>) -> Self {
        let mut canonical = BTreeMap::new();
        for (build_id, analysis) in &report.analyses {
            for item in analysis.all_items() {
                if let Some(path) = report.get_source_path(build_id, item.loc.source_id)
                    && let Some(source) = source_paths.get(path)
                {
                    let item = AttributionItem::new(source, item);
                    canonical.entry(item.key()).or_insert(item);
                }
            }
        }
        let mut items = Vec::with_capacity(canonical.len());
        let mut item_ids = HashMap::with_capacity_and_hasher(canonical.len(), Default::default());
        for (key, item) in canonical {
            item_ids.insert(key, items.len());
            items.push(item);
        }
        Self { report, source_paths, items, item_ids }
    }

    fn item_id(&self, build_id: &str, item: &CoverageItem) -> Option<usize> {
        let path = self.report.get_source_path(build_id, item.loc.source_id)?;
        let source = self.source_paths.get(path)?;
        self.item_ids.get(&AttributionItem::new(source, item).key()).copied()
    }
}

fn attributed_items<'a>(
    metadata: &AttributionIndex<'a>,
    resolved_hit_maps: &ResolvedHitMaps,
    instrumented_index: Option<&instrumented::InstrumentedCoverageTagIndex>,
    result: &TestResult,
) -> Vec<AttributionItem<'a>> {
    let report = metadata.report;
    let mut items = BTreeMap::<usize, u32>::new();
    let mut add_item = |build_id: &str, item: &CoverageItem, hits: u32| {
        if let Some(id) = metadata.item_id(build_id, item) {
            let count = items.entry(id).or_default();
            *count = count.saturating_add(hits);
        }
    };

    if let Some(index) = instrumented_index {
        for ((build_id, item_id), hits) in
            instrumented::item_hits(index, result.instrumented_coverage())
        {
            if let Some(analysis) = report.analyses.get(build_id)
                && let Some(item) = analysis.all_items().get(item_id as usize)
            {
                add_item(build_id, item, hits);
            }
        }
    } else if let Some(hit_maps) = &result.line_coverage {
        for (code_hash, map) in &hit_maps.0 {
            let Some(resolved) = resolved_hit_maps.get(code_hash) else { continue };
            for (item, hits) in
                report.hit_items_for_hit_map(&resolved.contract_id, map, resolved.is_deployed_code)
            {
                add_item(&resolved.contract_id.build_id, item, hits);
            }
        }
    }

    items.into_iter().map(|(id, hits)| AttributionItem { hits, ..metadata.items[id] }).collect()
}

fn coverage_item_kind_fields(
    kind: &CoverageItemKind,
) -> (&'static str, Option<&str>, Option<u32>, Option<u32>) {
    match kind {
        CoverageItemKind::Line => ("line", None, None, None),
        CoverageItemKind::Statement => ("statement", None, None, None),
        CoverageItemKind::Branch { branch_id, path_id, .. } => {
            ("branch", None, Some(*branch_id), Some(*path_id))
        }
        CoverageItemKind::Function { name } => ("function", Some(name), None, None),
    }
}

const fn test_status_name(status: TestStatus) -> &'static str {
    match status {
        TestStatus::Success => "success",
        TestStatus::Failure => "failure",
        TestStatus::Skipped => "skipped",
    }
}

const fn test_kind_name(kind: &TestKind) -> &'static str {
    match kind {
        TestKind::Unit { .. } => "unit",
        TestKind::Fuzz { .. } => "fuzz",
        TestKind::Invariant { .. } => "invariant",
        TestKind::Table { .. } => "table",
        TestKind::Symbolic { .. } => "symbolic",
        TestKind::Replay { .. } => "replay",
    }
}

#[derive(Serialize)]
struct IstanbulFileCoverage {
    path: String,
    #[serde(rename = "statementMap")]
    statement_map: BTreeMap<String, IstanbulLocation>,
    #[serde(rename = "fnMap")]
    function_map: BTreeMap<String, IstanbulFunction>,
    #[serde(rename = "branchMap")]
    branch_map: BTreeMap<String, IstanbulBranch>,
    #[serde(rename = "s")]
    statements: BTreeMap<String, u32>,
    #[serde(rename = "f")]
    functions: BTreeMap<String, u32>,
    #[serde(rename = "b")]
    branches: BTreeMap<String, Vec<u32>>,
}

#[derive(Clone, Serialize)]
struct IstanbulLocation {
    start: IstanbulPosition,
    end: IstanbulPosition,
}

#[derive(Clone, Serialize)]
struct IstanbulPosition {
    line: u32,
    column: u32,
}

#[derive(Serialize)]
struct IstanbulFunction {
    name: String,
    decl: IstanbulLocation,
    loc: IstanbulLocation,
    line: u32,
}

#[derive(Serialize)]
struct IstanbulBranch {
    loc: IstanbulLocation,
    #[serde(rename = "type")]
    branch_type: &'static str,
    locations: Vec<IstanbulLocation>,
}

/// A super verbose reporter for debugging coverage while it is still unstable.
pub struct DebugReporter;

impl CoverageReporter for DebugReporter {
    fn name(&self) -> &'static str {
        "debug"
    }

    fn report(&mut self, report: &CoverageReport) -> eyre::Result<()> {
        for (path, items) in report.items_by_file() {
            let src = fs::read_to_string(path)?;
            sh_println!("{}:", path.display())?;
            for item in items {
                sh_println!("- {}", item.fmt_with_source(Some(&src)))?;
            }
            sh_println!()?;
        }

        for (contract_id, (cta, rta)) in &report.anchors {
            if cta.is_empty() && rta.is_empty() {
                continue;
            }

            let anchors = cta
                .iter()
                .map(|anchor| (false, anchor))
                .chain(rta.iter().map(|anchor| (true, anchor)))
                .filter_map(|(is_runtime, anchor)| {
                    let item = report
                        .analyses
                        .get(&contract_id.build_id)
                        .and_then(|items| items.get(anchor.item_id))?;
                    // Source filters retain analyses to keep anchor item IDs stable, so debug
                    // output must apply the same reportable-source filter as other reporters.
                    report
                        .get_source_path(&contract_id.build_id, item.loc.source_id)
                        .is_some()
                        .then_some((is_runtime, anchor, item))
                })
                .collect::<Vec<_>>();
            if anchors.is_empty() {
                continue;
            }

            sh_println!("Anchors for {contract_id}:")?;
            for (is_runtime, anchor, item) in anchors {
                let kind = if is_runtime { " runtime" } else { "creation" };
                sh_println!("- {kind} {anchor}: {item}")?;
            }
            sh_println!()?;
        }

        Ok(())
    }
}

/// Cache line/column offsets for Istanbul JSON source locations.
struct SourcePositionCache {
    root: PathBuf,
    line_offsets: HashMap<PathBuf, Vec<usize>>,
}

impl SourcePositionCache {
    fn new(root: PathBuf) -> Self {
        Self { root, line_offsets: HashMap::default() }
    }

    fn location(&mut self, path: &Path, loc: &SourceLocation) -> eyre::Result<IstanbulLocation> {
        Ok(IstanbulLocation {
            start: self.position(path, loc.bytes.start as usize, loc.lines.start)?,
            end: self.position(path, loc.bytes.end as usize, loc.lines.end.saturating_sub(1))?,
        })
    }

    fn position(
        &mut self,
        path: &Path,
        offset: usize,
        fallback_line: u32,
    ) -> eyre::Result<IstanbulPosition> {
        let line_offsets = match self.line_offsets.entry(path.to_path_buf()) {
            hash_map::Entry::Occupied(o) => o.into_mut(),
            hash_map::Entry::Vacant(v) => {
                let source_path =
                    if path.is_absolute() { path.to_path_buf() } else { self.root.join(path) };
                let text = fs::read_to_string(source_path)?;
                let mut line_offsets = vec![0];
                for (idx, byte) in text.bytes().enumerate() {
                    if byte == b'\n' {
                        line_offsets.push(idx + 1);
                    }
                }
                v.insert(line_offsets)
            }
        };

        let line_index = match line_offsets.binary_search(&offset) {
            Ok(index) => index,
            Err(0) => {
                return Ok(IstanbulPosition { line: fallback_line.max(1), column: 0 });
            }
            Err(index) => index - 1,
        };
        let line = line_index as u32 + 1;
        let column = offset.saturating_sub(line_offsets[line_index]) as u32;
        Ok(IstanbulPosition { line, column })
    }
}

pub struct BytecodeReporter {
    root: PathBuf,
    destdir: PathBuf,
}

impl BytecodeReporter {
    pub const fn new(root: PathBuf, destdir: PathBuf) -> Self {
        Self { root, destdir }
    }
}

impl CoverageReporter for BytecodeReporter {
    fn name(&self) -> &'static str {
        "bytecode"
    }

    fn needs_source_maps(&self) -> bool {
        true
    }

    fn report(&mut self, report: &CoverageReport) -> eyre::Result<()> {
        use std::fmt::Write;

        fs::create_dir_all(&self.destdir)?;

        let no_source_elements = Vec::new();
        let mut line_number_cache = LineNumberCache::new(self.root.clone());

        for (contract_id, hits) in &report.bytecode_hits {
            let ops = disassemble_bytes(hits.bytecode().to_vec())?;
            let mut formatted = String::new();

            let source_elements =
                report.source_maps.get(contract_id).map(|sm| &sm.1).unwrap_or(&no_source_elements);

            for (code, source_element) in std::iter::zip(ops.iter(), source_elements) {
                let hits = hits
                    .get(code.offset)
                    .map(|h| format!("[{h:03}]"))
                    .unwrap_or("     ".to_owned());
                let source_id = source_element.index();
                let source_path = source_id
                    .and_then(|i| report.get_source_path(&contract_id.build_id, i as usize));

                let code = format!("{code:?}");
                let start = source_element.offset() as usize;
                let end = (source_element.offset() + source_element.length()) as usize;

                if let Some(source_path) = source_path {
                    let (sline, spos) = line_number_cache.get_position(source_path, start)?;
                    let (eline, epos) = line_number_cache.get_position(source_path, end)?;
                    writeln!(
                        formatted,
                        "{} {:40} // {}: {}:{}-{}:{} ({}-{})",
                        hits,
                        code,
                        source_path.display(),
                        sline,
                        spos,
                        eline,
                        epos,
                        start,
                        end
                    )?;
                } else if let Some(source_id) = source_id {
                    writeln!(formatted, "{hits} {code:40} // SRCID{source_id}: ({start}-{end})")?;
                } else {
                    writeln!(formatted, "{hits} {code:40}")?;
                }
            }
            fs::write(
                self.destdir.join(&*contract_id.contract_name).with_extension("asm"),
                formatted,
            )?;
        }

        Ok(())
    }
}

/// Cache line number offsets for source files
struct LineNumberCache {
    root: PathBuf,
    line_offsets: HashMap<PathBuf, Vec<usize>>,
}

impl LineNumberCache {
    pub fn new(root: PathBuf) -> Self {
        Self { root, line_offsets: HashMap::default() }
    }

    pub fn get_position(&mut self, path: &Path, offset: usize) -> eyre::Result<(usize, usize)> {
        let line_offsets = match self.line_offsets.entry(path.to_path_buf()) {
            hash_map::Entry::Occupied(o) => o.into_mut(),
            hash_map::Entry::Vacant(v) => {
                let text = fs::read_to_string(self.root.join(path))?;
                let mut line_offsets = vec![0];
                for line in text.lines() {
                    let line_offset = line.as_ptr() as usize - text.as_ptr() as usize;
                    line_offsets.push(line_offset);
                }
                v.insert(line_offsets)
            }
        };
        let lo = match line_offsets.binary_search(&offset) {
            Ok(lo) => lo,
            Err(lo) => lo - 1,
        };
        let pos = offset - line_offsets.get(lo).unwrap() + 1;
        Ok((lo, pos))
    }
}
