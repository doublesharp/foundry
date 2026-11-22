//! Solidity source instrumentation and metadata for coverage probes.

use alloy_primitives::{B256, Keccak256};
use eyre::Result;
use foundry_common::errors::convert_solar_errors;
use foundry_compilers::{
    Compiler, ProjectPathsConfig, SourceParser,
    artifacts::{SolcLanguage, Source},
    buildinfo::SourceEdit,
    error::{self, SolcError},
    multi::{MultiCompiler, MultiCompilerInput, MultiCompilerLanguage},
    project::Preprocessor,
    solc::{SolcCompiler, SolcVersionedInput},
};
use globset::GlobSet;
use rayon::prelude::*;
use semver::Version;
use solar::{
    ast::{
        self, BinOpKind, ExprKind, FunctionKind, ItemKind, StateMutability, StmtKind, Visit,
        yul::{self, StmtKind as Yul},
    },
    data_structures::Never,
    interface::{RelativeBytePos, Span, source_map::SourceFile},
    parse::{Cursor, lexer::token::RawTokenKind},
};
use std::{
    collections::{BTreeMap, HashSet},
    fmt::Write as _,
    ops::{ControlFlow, Range},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

mod diagnostics;

/// Virtual Solidity source used by instrumented coverage builds.
pub const COVERAGE_LIBRARY_PATH: &str = "__foundry_coverage/InstrumentedCoverage.sol";

const COVERAGE_LIBRARY_SOURCE: &str = r#"// SPDX-License-Identifier: MIT
pragma solidity >=0.6.0 <0.9.0;

library __FoundryCoverage {
    address constant COVERAGE_ADDRESS = 0xc0bEc0BEc0BeC0bEC0beC0bEC0bEC0beC0beC0BE;

    function _sendHitImplementation(uint256 tag) private view {
        address coverageAddress = COVERAGE_ADDRESS;
        /// @solidity memory-safe-assembly
        assembly {
            let scratch := mload(0x00)
            mstore(0x00, tag)
            pop(staticcall(gas(), coverageAddress, 0x00, 0x20, 0, 0))
            mstore(0x00, scratch)
        }
    }

    function _castToPure(function(uint256) internal view fnIn)
        private
        pure
        returns (function(uint256) internal pure fnOut)
    {
        /// @solidity memory-safe-assembly
        assembly {
            fnOut := fnIn
        }
    }

    function sendHit(uint256 tag) internal pure {
        _castToPure(_sendHitImplementation)(tag);
    }

    /// Records a hit for `tag` and returns `value` unchanged, so the call can be embedded in a
    /// boolean expression to measure which side of a branch executed without altering the
    /// expression's value or its short-circuit evaluation order.
    function sendHitReturn(uint256 tag, bool value) internal pure returns (bool) {
        sendHit(tag);
        return value;
    }
}
"#;

const COVERAGE_IMPORT: &str =
    r#"import {__FoundryCoverage} from "__foundry_coverage/InstrumentedCoverage.sol";"#;

const IGNORE_FILE_DIRECTIVES: &[&str] = &[
    "forge coverage ignore file",
    "coverage ignore file",
    "istanbul ignore file",
    "solidity-coverage ignore file",
];

const IGNORE_NEXT_DIRECTIVES: &[&str] = &[
    "forge coverage ignore next",
    "coverage ignore next",
    "istanbul ignore next",
    "solidity-coverage ignore next",
];

/// Source location attached to an injected coverage tag.
#[derive(Clone, Debug)]
pub struct InstrumentedCoverageProbe {
    pub version: Version,
    pub path: PathBuf,
    pub contract_name: Box<str>,
    pub tag: B256,
    pub kind: InstrumentedCoverageProbeKind,
    pub bytes: Range<u32>,
    pub lines: Range<u32>,
}

/// Coverage item represented by an injected tag.
#[derive(Clone, Debug)]
pub enum InstrumentedCoverageProbeKind {
    Statement,
    Branch { branch_id: u32, path_id: u32 },
    RequirePre { branch_id: u32 },
    RequirePost { branch_id: u32 },
    Function { name: Box<str> },
}

/// Metadata produced by source instrumentation.
#[derive(Clone, Debug, Default)]
pub struct InstrumentedCoverageMetadata {
    pub probes: Vec<InstrumentedCoverageProbe>,
    /// Original text and instrumentation edits of every compiled source, keyed by compiler
    /// source name. Sources that were not instrumented have no edits.
    pub sources: BTreeMap<PathBuf, InstrumentedSource>,
}

/// A compiler input source before instrumentation.
#[derive(Clone, Debug)]
pub struct InstrumentedSource {
    /// Source text before instrumentation.
    pub original: Arc<String>,
    /// Insertions applied by instrumentation, sorted by original offset.
    pub edits: Vec<SourceEdit>,
}

/// Shared metadata populated while solc inputs are preprocessed.
pub type InstrumentedCoverageMetadataRef = Arc<Mutex<InstrumentedCoverageMetadata>>;

/// Source preprocessor used by `forge coverage --instrumented`.
#[derive(Clone, Debug)]
pub struct CoverageInstrumentationPreprocessor {
    metadata: InstrumentedCoverageMetadataRef,
    include_libs: bool,
    exclude_tests: bool,
    excluded_sources: Option<regex::Regex>,
    skip_files: Option<GlobSet>,
}

impl CoverageInstrumentationPreprocessor {
    pub const fn new(
        metadata: InstrumentedCoverageMetadataRef,
        include_libs: bool,
        exclude_tests: bool,
        excluded_sources: Option<regex::Regex>,
        skip_files: Option<GlobSet>,
    ) -> Self {
        Self { metadata, include_libs, exclude_tests, excluded_sources, skip_files }
    }

    fn preprocess_solc(
        &self,
        _solc: &SolcCompiler,
        input: &mut SolcVersionedInput,
        paths: &ProjectPathsConfig<SolcLanguage>,
        _mocks: &mut HashSet<PathBuf>,
    ) -> error::Result<()> {
        let mut instrumented_sources = BTreeMap::new();
        let mut sources = input
            .input
            .sources
            .iter()
            .filter(|(path, _)| path.as_path() != Path::new(COVERAGE_LIBRARY_PATH))
            .map(|(path, source)| {
                let source =
                    InstrumentedSource { original: source.content.clone(), edits: Vec::new() };
                (path.clone(), source)
            })
            .collect::<BTreeMap<_, _>>();

        let candidate_paths = input
            .input
            .sources
            .keys()
            .filter(|path| self.should_instrument(paths, path))
            .cloned()
            .collect::<Vec<_>>();

        if !candidate_paths.is_empty() {
            let mut compiler =
                foundry_compilers::resolver::parse::SolParser::new(paths.with_language_ref())
                    .into_compiler();
            let instrumentation_result =
                compiler.enter_mut(|compiler| -> solar::interface::Result {
                    let mut pcx = compiler.parse();
                    let source_map = compiler.sess().source_map();

                    for path in &candidate_paths {
                        let Some(source) = input.input.sources.get(path) else { continue };
                        if let Ok(src_file) =
                            source_map.new_source_file_shared(path.clone(), source.content.clone())
                        {
                            pcx.add_file(src_file);
                        }
                    }

                    pcx.parse();
                    let gcx = compiler.gcx();
                    let results = candidate_paths
                        .par_iter()
                        .filter_map(|path| {
                            let source = input.input.sources.get(path)?;
                            let (_, ast_source) = gcx.get_ast_source(path)?;
                            let ast = ast_source.ast.as_ref()?;
                            let result = instrument_source(
                                path,
                                source.content.as_str(),
                                &input.version,
                                &ast_source.file,
                                ast,
                            )?;
                            Some((path.clone(), result))
                        })
                        .collect::<Vec<_>>();
                    instrumented_sources.extend(results);

                    Ok(())
                });

            if let Err(err) = convert_solar_errors(compiler.dcx()) {
                return Err(SolcError::msg(format!("failed coverage instrumentation: {err}")));
            }
            if let Err(err) = instrumentation_result {
                return Err(SolcError::msg(format!("failed coverage instrumentation: {err:?}")));
            }
        }

        let mut probes = Vec::new();
        let mut changed = false;

        for (path, (content, mut source_probes, edits)) in instrumented_sources {
            let Some(source) = input.input.sources.get_mut(&path) else { continue };
            source.content = Arc::new(content);
            changed = true;
            probes.append(&mut source_probes);
            if let Some(source) = sources.get_mut(&path) {
                source.edits = edits;
            }
        }

        if changed {
            input
                .input
                .sources
                .insert(PathBuf::from(COVERAGE_LIBRARY_PATH), Source::new(COVERAGE_LIBRARY_SOURCE));
        }

        let mut metadata = self.metadata.lock().unwrap_or_else(|e| e.into_inner());
        metadata.probes.extend(probes);
        // Every compiler job instruments a shared source identically.
        for (path, source) in sources {
            metadata.sources.entry(path).or_insert(source);
        }
        Ok(())
    }

    fn should_instrument(&self, paths: &ProjectPathsConfig<SolcLanguage>, path: &Path) -> bool {
        if path == Path::new(COVERAGE_LIBRARY_PATH) {
            return false;
        }
        if path.extension().and_then(|s| s.to_str()).is_none_or(|ext| ext != "sol") {
            return false;
        }
        if !self.include_libs && paths.has_library_ancestor(path) {
            return false;
        }
        if self.exclude_tests && paths.is_test(path) {
            return false;
        }
        let relative_path = path.strip_prefix(&paths.root).unwrap_or(path);
        if let Some(excluded_sources) = &self.excluded_sources
            && excluded_sources.is_match(&relative_path.to_string_lossy())
        {
            return false;
        }
        if let Some(skip_files) = &self.skip_files
            && skip_files.is_match(relative_path)
        {
            return false;
        }
        true
    }
}

impl Preprocessor<SolcCompiler> for CoverageInstrumentationPreprocessor {
    #[instrument(name = "CoverageInstrumentationPreprocessor::preprocess", skip_all)]
    fn preprocess(
        &self,
        solc: &SolcCompiler,
        input: &mut SolcVersionedInput,
        paths: &ProjectPathsConfig<SolcLanguage>,
        mocks: &mut HashSet<PathBuf>,
    ) -> error::Result<()> {
        self.preprocess_solc(solc, input, paths, mocks)
    }
}

impl Preprocessor<MultiCompiler> for CoverageInstrumentationPreprocessor {
    fn preprocess(
        &self,
        compiler: &MultiCompiler,
        input: &mut <MultiCompiler as Compiler>::Input,
        paths: &ProjectPathsConfig<MultiCompilerLanguage>,
        mocks: &mut HashSet<PathBuf>,
    ) -> error::Result<()> {
        let MultiCompilerInput::Solc(input) = input else { return Ok(()) };
        let Some(solc) = &compiler.solc else { return Ok(()) };
        let paths = paths.clone().with_language::<SolcLanguage>();
        self.preprocess_solc(solc, input, &paths, mocks)
    }
}

fn instrument_source<'ast>(
    path: &Path,
    source: &str,
    version: &Version,
    source_file: &SourceFile,
    ast: &'ast ast::SourceUnit<'ast>,
) -> Option<(String, Vec<InstrumentedCoverageProbe>, Vec<SourceEdit>)> {
    let ignore_next_comments = scan_ignore_directives(source)?;

    let mut collector =
        StatementCollector::new(path, source, version, source_file, ignore_next_comments);
    let _ = ast::Visit::visit_source_unit(&mut collector, ast);
    if collector.probes.is_empty() {
        return None;
    }

    let mut content = String::with_capacity(
        source.len() + collector.updates.iter().map(|update| update.text.len()).sum::<usize>(),
    );
    let mut cursor = 0;
    let mut updates = collector.updates.iter().enumerate().collect::<Vec<_>>();
    // Order updates sharing a byte offset so nested wrappers brace correctly: closes precede
    // points precede opens; among closes the innermost (last pushed) emits first; among opens
    // the outermost (first pushed) emits first. See [`Edge`].
    updates.sort_by(|(a_idx, a), (b_idx, b)| {
        a.range
            .start
            .cmp(&b.range.start)
            .then_with(|| edge_rank(a.edge).cmp(&edge_rank(b.edge)))
            .then_with(|| match a.edge {
                Edge::Close => b_idx.cmp(a_idx),
                Edge::Open | Edge::Point => a_idx.cmp(b_idx),
            })
    });
    let mut edits = Vec::with_capacity(updates.len() + 1);
    for (_, update) in updates {
        content.push_str(&source[cursor..update.range.start]);
        content.push_str(&update.text);
        cursor = update.range.end;
        edits.push(SourceEdit {
            start: update.range.start,
            end: update.range.end,
            len: update.text.len(),
        });
    }
    content.push_str(&source[cursor..]);
    let import_start = content.len();
    content.push('\n');
    content.push_str(COVERAGE_IMPORT);
    content.push('\n');
    edits.push(SourceEdit {
        start: source.len(),
        end: source.len(),
        len: content.len() - import_start,
    });
    Some((content, collector.probes, edits))
}

fn comment_ranges(source: &str) -> impl Iterator<Item = Range<usize>> {
    Cursor::new(source).with_position().filter_map(|(start, token)| {
        matches!(token.kind, RawTokenKind::LineComment { .. } | RawTokenKind::BlockComment { .. })
            .then_some(start..start + token.len as usize)
    })
}

/// ASCII case-insensitive substring check without heap allocations.
fn contains_ascii_case_insensitive(haystack: &str, needle_lower: &str) -> bool {
    let haystack = haystack.as_bytes();
    let needle = needle_lower.as_bytes();
    if needle.is_empty() {
        return true;
    }
    if haystack.len() < needle.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|window| window.eq_ignore_ascii_case(needle))
}

/// Scans `source` once for coverage ignore directives.
///
/// Returns `None` if the file contains a file-level ignore directive, or `Some(ignore_next)` with
/// the sorted byte ranges of all `ignore next` comment directives.
fn scan_ignore_directives(source: &str) -> Option<Vec<Range<usize>>> {
    // Fast path: skip lexing completely when the file does not mention "ignore" anywhere.
    if !contains_ascii_case_insensitive(source, "ignore") {
        return Some(Vec::new());
    }
    let mut ignore_next = Vec::new();
    for range in comment_ranges(source) {
        let text = &source[range.clone()];
        if !contains_ascii_case_insensitive(text, "ignore") {
            continue;
        }
        if contains_ignore_directive(text, IGNORE_FILE_DIRECTIVES) {
            return None;
        }
        if contains_ignore_directive(text, IGNORE_NEXT_DIRECTIVES) {
            ignore_next.push(range);
        }
    }
    Some(ignore_next)
}

#[cfg(test)]
fn has_ignore_file_directive(source: &str) -> bool {
    scan_ignore_directives(source).is_none()
}

fn contains_ignore_directive(text: &str, directives: &[&str]) -> bool {
    directives.iter().any(|directive| contains_ascii_case_insensitive(text, directive))
}

/// Sort rank for an [`Edge`] at a shared byte offset: closes first, then points, then opens.
const fn edge_rank(edge: Edge) -> u8 {
    match edge {
        Edge::Close => 0,
        Edge::Point => 1,
        Edge::Open => 2,
    }
}

struct StatementCollector<'a> {
    path: &'a Path,
    source: &'a str,
    version: &'a Version,
    source_file: &'a SourceFile,
    /// Pre-seeded `Keccak256` hasher with `"foundry-coverage:{version}:{path}:"`.
    tag_hasher: Keccak256,
    updates: Vec<SourceUpdate>,
    probes: Vec<InstrumentedCoverageProbe>,
    /// Monotonic counter mixed into every tag so each probe gets a unique tag regardless of its
    /// source position (several probes can share a byte offset, e.g. a require's pre/post or a
    /// ternary's two condition tags).
    next_tag: u64,
    next_branch: u32,
    contract_name: Box<str>,
    probe_mode: ProbeMode,
    yul_scratch_name: String,
    ignore_next_comments: Vec<Range<usize>>,
}

impl<'a> StatementCollector<'a> {
    fn new(
        path: &'a Path,
        source: &'a str,
        version: &'a Version,
        source_file: &'a SourceFile,
        ignore_next_comments: Vec<Range<usize>>,
    ) -> Self {
        // Yul forbids shadowing even inside a nested block.
        let mut yul_scratch_name = "__foundry_coverage_scratch".to_string();
        while source.contains(&yul_scratch_name) {
            yul_scratch_name.push('_');
        }
        let mut tag_hasher = Keccak256::new();
        let prefix = format!("foundry-coverage:{}:{}:", version, path.display());
        tag_hasher.update(prefix.as_bytes());
        Self {
            path,
            source,
            version,
            source_file,
            tag_hasher,
            updates: Vec::new(),
            probes: Vec::new(),
            next_tag: 0,
            next_branch: 0,
            contract_name: "".into(),
            probe_mode: ProbeMode::Direct,
            yul_scratch_name,
            ignore_next_comments,
        }
    }

    fn push_probe(&mut self, span: Span) {
        if self.has_ignore_next(span) {
            return;
        }
        self.push_probe_kind(InstrumentedCoverageProbeKind::Statement, span);
    }

    fn push_require_branch(&mut self, stmt: &'_ ast::Stmt<'_>) {
        let range = self.trim_statement_span(stmt.span);
        if range.is_empty() || self.has_ignore_next(stmt.span) {
            return;
        }

        let full_range = self.source_range(stmt.span);
        let branch_id = self.next_branch_id();

        let pre_tag = self.make_tag(range.start);
        self.updates.push(SourceUpdate::point(range.start, self.probe_text(pre_tag)));
        self.record_probe(
            InstrumentedCoverageProbeKind::RequirePre { branch_id },
            stmt.span,
            range.clone(),
            pre_tag,
        );

        let post_tag = self.make_tag(range.end);
        self.updates.push(SourceUpdate::point(full_range.end, self.probe_text(post_tag)));
        self.record_probe(
            InstrumentedCoverageProbeKind::RequirePost { branch_id },
            stmt.span,
            range,
            post_tag,
        );
    }

    fn push_probe_kind(&mut self, kind: InstrumentedCoverageProbeKind, span: Span) {
        let range = self.trim_statement_span(span);
        if range.is_empty() || self.has_ignore_next(span) {
            return;
        }
        let tag = self.make_tag(range.start);
        let text = self.probe_text(tag);
        self.updates.push(SourceUpdate::point(range.start, text));
        self.record_probe(kind, span, range, tag);
    }

    /// Wraps a non-block statement in `{ <probe> stmt }` so a probe can be attached without
    /// changing control flow, e.g. for a single-statement `if`/loop body. `trailing`, if present,
    /// is appended after the closing brace within the same insertion (used for a synthesized
    /// ` else { .. }`).
    fn wrap_and_probe(
        &mut self,
        stmt: &'_ ast::Stmt<'_>,
        kind: InstrumentedCoverageProbeKind,
        trailing: Option<String>,
    ) {
        let range = self.trim_statement_span(stmt.span);
        if range.is_empty() || self.has_ignore_next(stmt.span) {
            return;
        }
        let full_range = self.source_range(stmt.span);
        let tag = self.make_tag(range.start);
        self.updates
            .push(SourceUpdate::open(full_range.start, format!("{{ {}", self.probe_text(tag))));
        let close = match trailing {
            Some(trailing) => format!(" }}{trailing}"),
            None => " }".to_string(),
        };
        self.updates.push(SourceUpdate::close(full_range.end, close));
        self.record_probe(kind, stmt.span, range, tag);
    }

    /// Wraps a brace-less `require` body in `{ <body-probe> <pre> require(..); <post> }`, with the
    /// body-entry probe (`kind`) and the require's pre/post branch probes all fused into the
    /// synthesized block's braces. This keeps the braces attached (so an enclosing `if`/loop binds
    /// to the block, not to a detached probe) while still measuring both the body execution and
    /// the require's true/false paths.
    fn wrap_require_body(
        &mut self,
        stmt: &'_ ast::Stmt<'_>,
        kind: InstrumentedCoverageProbeKind,
        trailing: Option<String>,
    ) {
        let range = self.trim_statement_span(stmt.span);
        if range.is_empty() || self.has_ignore_next(stmt.span) {
            return;
        }
        let full_range = self.source_range(stmt.span);
        let branch_id = self.next_branch_id();

        // Emit a distinct body-entry probe only for a branch body (e.g. an `if` then/else path).
        // For a plain statement body (a loop body), the require's own pre-probe already marks the
        // statement at the same source range, so a separate statement probe would double-count it.
        let body_tag = matches!(kind, InstrumentedCoverageProbeKind::Branch { .. })
            .then(|| self.make_tag(range.start));
        let pre_tag = self.make_tag(range.start);
        let body_probe = body_tag.map(|tag| self.probe_text(tag)).unwrap_or_default();
        self.updates.push(SourceUpdate::open(
            full_range.start,
            format!("{{ {body_probe}{}", self.probe_text(pre_tag)),
        ));
        // Require post-probe and the closing brace (plus any trailing `else`).
        let post_tag = self.make_tag(range.end);
        let close = match trailing {
            Some(trailing) => format!(" {} }}{trailing}", self.probe_text(post_tag)),
            None => format!(" {} }}", self.probe_text(post_tag)),
        };
        self.updates.push(SourceUpdate::close(full_range.end, close));

        if let Some(body_tag) = body_tag {
            self.record_probe(kind, stmt.span, range.clone(), body_tag);
        }
        self.record_probe(
            InstrumentedCoverageProbeKind::RequirePre { branch_id },
            stmt.span,
            range.clone(),
            pre_tag,
        );
        self.record_probe(
            InstrumentedCoverageProbeKind::RequirePost { branch_id },
            stmt.span,
            range,
            post_tag,
        );
    }

    fn push_block_entry_probe(
        &mut self,
        block: &ast::Block<'_>,
        kind: InstrumentedCoverageProbeKind,
    ) {
        let range = self.trim_statement_span(block.span);
        if range.is_empty() || self.has_ignore_next(block.span) {
            return;
        }
        let full_range = self.source_range(block.span);
        let Some(brace_offset) = self.source[full_range.clone()].find('{') else {
            return;
        };
        let insert_at = full_range.start + brace_offset + 1;
        let tag = self.make_tag(range.start);
        self.updates.push(SourceUpdate::point(insert_at, self.probe_text(tag)));
        self.record_probe(kind, block.span, range, tag);
    }

    /// Inserts a Yul coverage probe immediately before the Yul statement at `span` and records a
    /// probe of `kind` for it.
    fn push_yul_stmt_probe(&mut self, span: Span, kind: InstrumentedCoverageProbeKind) {
        let range = self.source_range(span);
        if range.is_empty() || self.has_ignore_next(span) {
            return;
        }
        let tag = self.make_tag(range.start);
        self.updates.push(SourceUpdate::point(range.start, self.yul_probe_text(tag)));
        self.record_probe(kind, span, range, tag);
    }

    /// Inserts a Yul coverage probe just inside the opening brace of a Yul block (e.g. an `if`
    /// body or a `switch` case body) and records a probe of `kind`.
    fn push_yul_block_entry_probe(
        &mut self,
        block: &yul::Block<'_>,
        kind: InstrumentedCoverageProbeKind,
    ) {
        let range = self.source_range(block.span);
        if self.has_ignore_next(block.span) {
            return;
        }
        let Some(brace_offset) = self.source[range.clone()].find('{') else {
            return;
        };
        let insert_at = range.start + brace_offset + 1;
        let tag = self.make_tag(range.start);
        self.updates.push(SourceUpdate::point(insert_at, self.yul_probe_text(tag)));
        self.record_probe(kind, block.span, range, tag);
    }

    fn record_probe(
        &mut self,
        kind: InstrumentedCoverageProbeKind,
        span: Span,
        range: Range<usize>,
        tag: B256,
    ) {
        let lines = self.line_range(span);
        self.probes.push(InstrumentedCoverageProbe {
            version: self.version.clone(),
            path: self.path.to_path_buf(),
            contract_name: self.contract_name.clone(),
            tag,
            kind,
            bytes: range.start as u32..range.end as u32,
            lines,
        });
    }

    fn probe_text(&self, tag: B256) -> String {
        match self.probe_mode {
            ProbeMode::Direct => format!(
                "/// @solidity memory-safe-assembly\nassembly {{ mstore(0x00, {tag}) pop(staticcall(gas(), 0xc0bEc0BEc0BeC0bEC0beC0bEC0bEC0beC0beC0BE, 0x00, 0x20, 0, 0)) }}\n"
            ),
            ProbeMode::Library => format!("__FoundryCoverage.sendHit({tag}); "),
        }
    }

    /// Saves and restores the scratch word around a probe inside a Yul block.
    fn yul_probe_text(&self, tag: B256) -> String {
        let scratch = &self.yul_scratch_name;
        format!(
            "{{ let {scratch} := mload(0x00) mstore(0x00, {tag}) pop(staticcall(gas(), 0xc0bEc0BEc0BeC0bEC0beC0bEC0bEC0beC0beC0BE, 0x00, 0x20, 0x00, 0x00)) mstore(0x00, {scratch}) }} "
        )
    }

    fn trim_statement_span(&self, span: Span) -> Range<usize> {
        let mut range = self.source_range(span);
        if let Some(snippet) = self.source.get(range.clone())
            && let Some(stripped) = snippet.strip_suffix(';')
        {
            range.end -= snippet.len() - stripped.trim_end().len();
        }
        range
    }

    fn line_range(&self, span: Span) -> Range<u32> {
        let bytes = self.source_range(span);
        let last_pos = if bytes.end > bytes.start { bytes.end - 1 } else { bytes.start };
        let first = self.source_file.lookup_line(RelativeBytePos::from_usize(bytes.start)).unwrap();
        let last = self.source_file.lookup_line(RelativeBytePos::from_usize(last_pos)).unwrap();
        first as u32 + 1..last as u32 + 2
    }

    fn has_ignore_next(&self, span: Span) -> bool {
        if self.ignore_next_comments.is_empty() {
            return false;
        }
        let range = self.source_range(span);
        self.has_ignore_directive_before(range.start)
    }

    fn has_ignore_directive_before(&self, start: usize) -> bool {
        if self.ignore_next_comments.is_empty() {
            return false;
        }
        let Some(before) = self.source.get(..start) else { return false };
        let line_start = before.rfind('\n').map_or(0, |idx| idx + 1);
        let previous = before[..line_start].trim_end_matches(['\r', '\n']);
        let previous_line_start = previous.rfind('\n').map_or(0, |idx| idx + 1);

        let first =
            self.ignore_next_comments.partition_point(|range| range.end <= previous_line_start);
        for range in &self.ignore_next_comments[first..] {
            if range.end > start {
                break;
            }
            if Cursor::new(&before[range.end..start]).all(|token| {
                matches!(
                    token.kind,
                    RawTokenKind::Whitespace
                        | RawTokenKind::LineComment { .. }
                        | RawTokenKind::BlockComment { .. }
                )
            }) {
                return true;
            }
        }
        false
    }

    fn make_tag(&mut self, start: usize) -> B256 {
        let seq = self.next_tag;
        self.next_tag += 1;
        let mut suffix = String::with_capacity(44);
        let _ = write!(suffix, "{start}:{seq}");
        let mut hasher = self.tag_hasher.clone();
        hasher.update(suffix.as_bytes());
        hasher.finalize()
    }

    const fn next_branch_id(&mut self) -> u32 {
        let branch_id = self.next_branch;
        self.next_branch += 1;
        branch_id
    }

    fn visit_control_body<'ast>(
        &mut self,
        stmt: &'ast ast::Stmt<'ast>,
        kind: InstrumentedCoverageProbeKind,
    ) -> ControlFlow<<Self as ast::Visit<'ast>>::BreakValue> {
        self.visit_control_body_with_trailing(stmt, kind, None)
    }

    /// Like [`visit_control_body`](Self::visit_control_body), but appends `trailing` text
    /// (e.g. a synthesized ` else { .. }`) immediately after the body's closing brace as part of
    /// the same closing insertion. Fusing it into one insert keeps it bound to the enclosing
    /// statement and correctly ordered against other scopes that close at the same offset.
    fn visit_control_body_with_trailing<'ast>(
        &mut self,
        stmt: &'ast ast::Stmt<'ast>,
        kind: InstrumentedCoverageProbeKind,
        trailing: Option<String>,
    ) -> ControlFlow<<Self as ast::Visit<'ast>>::BreakValue> {
        if self.has_ignore_next(stmt.span) {
            return ControlFlow::Continue(());
        }
        match &stmt.kind {
            StmtKind::Block(block) | StmtKind::UncheckedBlock(block) => {
                self.push_block_entry_probe(block, kind);
                if let Some(trailing) = trailing {
                    let end = self.source_range(block.span).end;
                    self.updates.push(SourceUpdate::close(end, trailing));
                }
                self.visit_stmt(stmt)
            }
            StmtKind::DoWhile(..)
            | StmtKind::For { .. }
            | StmtKind::If(..)
            | StmtKind::Try(_)
            | StmtKind::While(..) => {
                self.wrap_and_probe(stmt, kind, trailing);
                self.visit_stmt(stmt)
            }
            _ if Self::is_require_stmt(stmt) => {
                // A brace-less `require` body needs its own block plus the require pre/post
                // branch probes placed *inside* that block. Emitting the require probes as
                // separate insertions (as `push_require_branch` does) would detach the
                // synthesized brace, so wrap and probe it in one fused step.
                self.wrap_require_body(stmt, kind, trailing);
                self.walk_stmt(stmt)
            }
            _ => {
                self.wrap_and_probe(stmt, kind, trailing);
                if let StmtKind::Assembly(assembly) = &stmt.kind {
                    if matches!(self.probe_mode, ProbeMode::Direct) {
                        self.visit_yul_block(&assembly.block)?;
                    }
                    ControlFlow::Continue(())
                } else {
                    self.walk_stmt(stmt)
                }
            }
        }
    }

    fn visit_try_block<'ast>(
        &mut self,
        block: &'ast ast::Block<'ast>,
    ) -> ControlFlow<<Self as ast::Visit<'ast>>::BreakValue> {
        for stmt in block.stmts.iter() {
            self.visit_stmt(stmt)?;
        }
        ControlFlow::Continue(())
    }

    /// Visits each statement of a Yul block so it can be instrumented.
    fn visit_yul_block<'ast>(
        &mut self,
        block: &'ast yul::Block<'ast>,
    ) -> ControlFlow<<Self as ast::Visit<'ast>>::BreakValue> {
        for stmt in block.stmts.iter() {
            self.visit_yul_stmt(stmt)?;
        }
        ControlFlow::Continue(())
    }

    fn function_name(func: &ast::ItemFunction<'_>) -> Box<str> {
        if let Some(name) = func.header.name.as_ref() {
            return name.as_str().into();
        }
        match func.kind {
            FunctionKind::Constructor => "constructor".into(),
            FunctionKind::Fallback => "fallback".into(),
            FunctionKind::Receive => "receive".into(),
            FunctionKind::Function | FunctionKind::Modifier => unreachable!(),
        }
    }

    const fn should_instrument_function(func: &ast::ItemFunction<'_>) -> bool {
        !matches!(func.kind, FunctionKind::Receive) && func.body.is_some()
    }

    /// Returns the source byte range covered by `span`.
    fn source_range(&self, span: Span) -> Range<usize> {
        let start = (span.lo() - self.source_file.start_pos).to_usize();
        let end = (span.hi() - self.source_file.start_pos).to_usize();
        start..end
    }

    /// Instruments a logical `&&`/`||` expression with a mutually-exclusive probe on `lhs` so
    /// short-circuiting (`path_id: 0`) and evaluating `rhs` (`path_id: 1`) are measured
    /// independently, then recurses so nested logical/ternary operands are also instrumented.
    fn instrument_logical_branch<'ast>(
        &mut self,
        lhs: &'ast ast::Expr<'ast>,
        op: BinOpKind,
        rhs: &'ast ast::Expr<'ast>,
    ) -> ControlFlow<<Self as ast::Visit<'ast>>::BreakValue> {
        let lhs_range = self.source_range(lhs.span);
        let rhs_range = self.source_range(rhs.span);
        if !lhs_range.is_empty() && !rhs_range.is_empty() {
            let branch_id = self.next_branch_id();
            let short_circuit_tag = self.make_tag(lhs_range.start);
            let rhs_tag = self.make_tag(rhs_range.start);
            let (true_tag, false_tag) = match op {
                BinOpKind::Or => (short_circuit_tag, rhs_tag),
                BinOpKind::And => (rhs_tag, short_circuit_tag),
                _ => unreachable!("only logical && and || are instrumented"),
            };
            self.updates.push(SourceUpdate::open(lhs_range.start, "(("));
            self.updates.push(SourceUpdate::close(
                lhs_range.end,
                format!(
                    ") ? __FoundryCoverage.sendHitReturn({true_tag}, true) : __FoundryCoverage.sendHitReturn({false_tag}, false))"
                ),
            ));
            self.record_probe(
                InstrumentedCoverageProbeKind::Branch { branch_id, path_id: 0 },
                lhs.span,
                lhs_range,
                short_circuit_tag,
            );
            self.record_probe(
                InstrumentedCoverageProbeKind::Branch { branch_id, path_id: 1 },
                rhs.span,
                rhs_range,
                rhs_tag,
            );
        }
        self.visit_expr(lhs)?;
        self.visit_expr(rhs)
    }

    /// Wraps the condition in a boolean ternary so exactly one outcome is recorded, then
    /// recurses into the original condition and arms. The original arms retain their types.
    fn instrument_ternary_branch<'ast>(
        &mut self,
        cond: &'ast ast::Expr<'ast>,
        then_expr: &'ast ast::Expr<'ast>,
        else_expr: &'ast ast::Expr<'ast>,
    ) -> ControlFlow<<Self as ast::Visit<'ast>>::BreakValue> {
        let range = self.source_range(cond.span);
        if !range.is_empty() {
            let branch_id = self.next_branch_id();
            let true_tag = self.make_tag(range.start);
            let false_tag = self.make_tag(range.end);
            self.updates.push(SourceUpdate::open(range.start, "(("));
            self.updates.push(SourceUpdate::close(
                range.end,
                format!(") ? __FoundryCoverage.sendHitReturn({true_tag}, true) : __FoundryCoverage.sendHitReturn({false_tag}, false))"),
            ));
            self.record_probe(
                InstrumentedCoverageProbeKind::Branch { branch_id, path_id: 0 },
                cond.span,
                range.clone(),
                true_tag,
            );
            self.record_probe(
                InstrumentedCoverageProbeKind::Branch { branch_id, path_id: 1 },
                cond.span,
                range,
                false_tag,
            );
        }
        self.visit_expr(cond)?;
        self.visit_expr(then_expr)?;
        self.visit_expr(else_expr)
    }

    fn is_require_expr(expr: &ast::Expr<'_>) -> bool {
        let expr = expr.peel_parens();
        let ast::ExprKind::Call(callee, _) = &expr.kind else { return false };
        let ast::ExprKind::Ident(ident) = &callee.peel_parens().kind else { return false };
        ident.as_str() == "require"
    }

    fn is_require_stmt(stmt: &ast::Stmt<'_>) -> bool {
        let StmtKind::Expr(expr) = &stmt.kind else { return false };
        Self::is_require_expr(expr)
    }

    /// Returns `true` if `stmt` contains executable statements. An empty block (or an empty
    /// `assembly`/`unchecked` block) contains none. Mirrors the native analyzer so empty `if`
    /// bodies do not produce phantom branches.
    fn stmt_has_statements(stmt: &ast::Stmt<'_>) -> bool {
        match &stmt.kind {
            StmtKind::Block(block) | StmtKind::UncheckedBlock(block) => !block.stmts.is_empty(),
            StmtKind::Assembly(assembly) => !assembly.block.stmts.is_empty(),
            _ => true,
        }
    }
}

#[derive(Debug)]
struct SourceUpdate {
    range: Range<usize>,
    text: String,
    edge: Edge,
}

impl SourceUpdate {
    /// An insertion that opens a scope (e.g. `{` or a wrapping prefix) at `pos`.
    fn open(pos: usize, text: impl Into<String>) -> Self {
        Self { range: pos..pos, text: text.into(), edge: Edge::Open }
    }

    /// An insertion that closes a scope (e.g. `}` or a wrapping suffix) at `pos`.
    fn close(pos: usize, text: impl Into<String>) -> Self {
        Self { range: pos..pos, text: text.into(), edge: Edge::Close }
    }

    /// A standalone insertion (e.g. a statement probe) at `pos` that neither opens nor closes
    /// a scope.
    fn point(pos: usize, text: impl Into<String>) -> Self {
        Self { range: pos..pos, text: text.into(), edge: Edge::Point }
    }
}

/// Whether a [`SourceUpdate`] opens a scope, closes one, or is standalone. When several updates
/// share a byte offset this determines their relative order so nested wrappers brace correctly:
/// inner closes precede outer closes, outer opens precede inner opens, and closes precede opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Edge {
    Close,
    Point,
    Open,
}

#[derive(Clone, Copy, Debug)]
enum ProbeMode {
    Direct,
    Library,
}

impl<'ast> ast::Visit<'ast> for StatementCollector<'_> {
    type BreakValue = Never;

    fn visit_item_contract(
        &mut self,
        contract: &'ast ast::ItemContract<'ast>,
    ) -> ControlFlow<Self::BreakValue> {
        let previous_contract_name =
            std::mem::replace(&mut self.contract_name, contract.name.as_str().into());
        self.walk_item_contract(contract)?;
        self.contract_name = previous_contract_name;
        ControlFlow::Continue(())
    }

    fn visit_item(&mut self, item: &'ast ast::Item<'ast>) -> ControlFlow<Self::BreakValue> {
        match &item.kind {
            ItemKind::Contract(_) => self.walk_item(item)?,
            ItemKind::Function(func) => {
                if !Self::should_instrument_function(func) {
                    return ControlFlow::Continue(());
                }
                if self.has_ignore_next(item.span) {
                    return ControlFlow::Continue(());
                }
                let previous_probe_mode = self.probe_mode;
                self.probe_mode = if func.kind.is_modifier()
                    || matches!(func.header.state_mutability(), StateMutability::Pure)
                {
                    ProbeMode::Library
                } else {
                    ProbeMode::Direct
                };
                if let Some(body) = func.body.as_ref() {
                    self.push_block_entry_probe(
                        body,
                        InstrumentedCoverageProbeKind::Function { name: Self::function_name(func) },
                    );
                    // Visit only the body, not the function header. Expressions in modifier
                    // invocations, base-constructor arguments, and return types are not
                    // control-flow in the function and must not be instrumented as branches
                    // (this matches the native analyzer and keeps branch denominators stable).
                    for stmt in body.stmts.iter() {
                        self.visit_stmt(stmt)?;
                    }
                }
                self.probe_mode = previous_probe_mode;
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }

    fn visit_stmt(&mut self, stmt: &'ast ast::Stmt<'ast>) -> ControlFlow<Self::BreakValue> {
        if self.has_ignore_next(stmt.span) {
            return ControlFlow::Continue(());
        }

        match &stmt.kind {
            StmtKind::Break | StmtKind::Continue => {
                self.push_probe(stmt.span);
                ControlFlow::Continue(())
            }
            StmtKind::Assembly(assembly) => {
                // Count the `assembly { .. }` block itself as one statement (for the line).
                self.push_probe(stmt.span);
                // Descend into the Yul to instrument each inner statement and branch, but only in
                // `Direct` (non-pure) functions: the Yul probe issues a `staticcall`, which reads
                // the environment and is therefore rejected inside a `pure` function. In `pure`
                // and modifier contexts the assembly stays a single statement.
                if matches!(self.probe_mode, ProbeMode::Direct) {
                    self.visit_yul_block(&assembly.block)?;
                }
                ControlFlow::Continue(())
            }
            StmtKind::DeclSingle(_)
            | StmtKind::DeclMulti(..)
            | StmtKind::Emit(..)
            | StmtKind::Return(_)
            | StmtKind::Revert(..) => {
                self.push_probe(stmt.span);
                self.walk_stmt(stmt)
            }
            StmtKind::Expr(expr) if Self::is_require_expr(expr) => {
                self.push_require_branch(stmt);
                self.walk_expr(expr)
            }
            StmtKind::Expr(_) => {
                self.push_probe(stmt.span);
                self.walk_stmt(stmt)
            }
            StmtKind::Block(block) | StmtKind::UncheckedBlock(block) => {
                for stmt in block.stmts.iter() {
                    self.visit_stmt(stmt)?;
                }
                ControlFlow::Continue(())
            }
            StmtKind::If(condition, then_stmt, else_stmt) => {
                self.visit_expr(condition)?;

                // Only treat this as a branch if a body has executable statements, matching the
                // native analyzer; an `if (b) {}` with empty bodies yields no branch. Empty bodies
                // contain nothing to instrument, so there is nothing further to visit.
                let has_body = Self::stmt_has_statements(then_stmt)
                    || else_stmt.as_ref().is_some_and(|s| Self::stmt_has_statements(s));
                if !has_body {
                    return ControlFlow::Continue(());
                }

                let branch_id = self.next_branch_id();
                // The implicit else (`path_id: 1`) for a bare `if` is fused onto the then-body's
                // closing brace as a single insertion, so it binds to this `if` and orders
                // correctly relative to enclosing scopes even for nested brace-less `if`s.
                let implicit_else = (else_stmt.is_none() && !self.has_ignore_next(then_stmt.span))
                    .then(|| {
                        let then_range = self.source_range(then_stmt.span);
                        let tag = self.make_tag(then_range.end);
                        self.record_probe(
                            InstrumentedCoverageProbeKind::Branch { branch_id, path_id: 1 },
                            then_stmt.span,
                            then_range,
                            tag,
                        );
                        format!(" else {{ {}}}", self.probe_text(tag))
                    });
                self.visit_control_body_with_trailing(
                    then_stmt,
                    InstrumentedCoverageProbeKind::Branch { branch_id, path_id: 0 },
                    implicit_else,
                )?;
                if let Some(else_stmt) = else_stmt {
                    self.visit_control_body(
                        else_stmt,
                        InstrumentedCoverageProbeKind::Branch { branch_id, path_id: 1 },
                    )?;
                }
                ControlFlow::Continue(())
            }
            StmtKind::Try(try_) => {
                self.visit_expr(try_.expr)?;
                let branch_id = self.next_branch_id();
                for (path_id, clause) in try_.clauses.iter().enumerate() {
                    if self.has_ignore_next(clause.span) || self.has_ignore_next(clause.block.span)
                    {
                        continue;
                    }
                    self.push_block_entry_probe(
                        &clause.block,
                        InstrumentedCoverageProbeKind::Branch {
                            branch_id,
                            path_id: path_id as u32,
                        },
                    );
                    self.visit_try_block(&clause.block)?;
                }
                ControlFlow::Continue(())
            }
            StmtKind::While(condition, body) => {
                self.visit_expr(condition)?;
                self.visit_control_body(body, InstrumentedCoverageProbeKind::Statement)?;
                ControlFlow::Continue(())
            }
            StmtKind::DoWhile(body, condition) => {
                self.visit_control_body(body, InstrumentedCoverageProbeKind::Statement)?;
                self.visit_expr(condition)?;
                ControlFlow::Continue(())
            }
            StmtKind::For { init, cond, next, body } => {
                if let Some(init) = init {
                    // Walk child expressions (such as ternaries or logical operators) without
                    // injecting a statement probe inside the `for (...)` header.
                    self.walk_stmt(init)?;
                }
                if let Some(cond) = cond {
                    self.visit_expr(cond)?;
                }
                if let Some(next) = next {
                    self.visit_expr(next)?;
                }
                self.visit_control_body(body, InstrumentedCoverageProbeKind::Statement)?;
                ControlFlow::Continue(())
            }
            StmtKind::Placeholder => ControlFlow::Continue(()),
        }
    }

    fn visit_ty(&mut self, _ty: &'ast ast::Type<'ast>) -> ControlFlow<Self::BreakValue> {
        // Type expressions such as fixed-size array lengths (`uint256[FLAG ? 2 : 4]`) must stay
        // compile-time constants and cannot contain coverage probes.
        ControlFlow::Continue(())
    }

    fn visit_expr(&mut self, expr: &'ast ast::Expr<'ast>) -> ControlFlow<Self::BreakValue> {
        match &expr.kind {
            ExprKind::Binary(lhs, op, rhs) if matches!(op.kind, BinOpKind::And | BinOpKind::Or) => {
                self.instrument_logical_branch(lhs, op.kind, rhs)
            }
            ExprKind::Ternary(cond, then_expr, else_expr) => {
                self.instrument_ternary_branch(cond, then_expr, else_expr)
            }
            // Not a branch on its own; descend so any nested logical/ternary operands are found.
            _ => self.walk_expr(expr),
        }
    }

    fn visit_yul_stmt(&mut self, stmt: &'ast yul::Stmt<'ast>) -> ControlFlow<Self::BreakValue> {
        if self.has_ignore_next(stmt.span) {
            return ControlFlow::Continue(());
        }

        match &stmt.kind {
            // Simple statements: count each one and insert a probe before it.
            Yul::VarDecl(..)
            | Yul::AssignSingle(..)
            | Yul::AssignMulti(..)
            | Yul::Expr(_)
            | Yul::Leave
            | Yul::Break
            | Yul::Continue => {
                self.push_yul_stmt_probe(stmt.span, InstrumentedCoverageProbeKind::Statement);
                ControlFlow::Continue(())
            }
            // `if cond { body }`: a one-sided branch, probed at the body's entry.
            Yul::If(_, body) => {
                let branch_id = self.next_branch_id();
                self.push_yul_block_entry_probe(
                    body,
                    InstrumentedCoverageProbeKind::Branch { branch_id, path_id: 0 },
                );
                self.visit_yul_block(body)
            }
            // `switch`: each case body (including default) is a branch path.
            Yul::Switch(switch) => {
                let branch_id = self.next_branch_id();
                for (path_id, case) in switch.cases.iter().enumerate() {
                    if self.has_ignore_next(case.span) || self.has_ignore_next(case.body.span) {
                        continue;
                    }
                    self.push_yul_block_entry_probe(
                        &case.body,
                        InstrumentedCoverageProbeKind::Branch {
                            branch_id,
                            path_id: path_id as u32,
                        },
                    );
                    self.visit_yul_block(&case.body)?;
                }
                ControlFlow::Continue(())
            }
            // `for { init } cond { step } { body }`: probe the body entry as a statement; recurse
            // into init/step/body so their inner statements are instrumented too.
            Yul::For(for_) => {
                self.visit_yul_block(&for_.init)?;
                self.push_yul_block_entry_probe(
                    &for_.body,
                    InstrumentedCoverageProbeKind::Statement,
                );
                self.visit_yul_block(&for_.body)?;
                self.visit_yul_block(&for_.step)
            }
            Yul::FunctionDef(func) => {
                self.push_yul_block_entry_probe(
                    &func.body,
                    InstrumentedCoverageProbeKind::Function { name: func.name.as_str().into() },
                );
                self.visit_yul_block(&func.body)
            }
            Yul::Block(block) => self.visit_yul_block(block),
        }
    }
}

pub fn read_metadata(
    metadata: &InstrumentedCoverageMetadataRef,
) -> Result<InstrumentedCoverageMetadata> {
    Ok(metadata.lock().unwrap_or_else(|e| e.into_inner()).clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use solar::{
        interface::{Session, source_map::FileName},
        sema::Compiler,
    };

    /// Parses `source` with solar and runs the coverage instrumenter on it, returning the
    /// rewritten source and the probes that were recorded.
    ///
    /// This exercises the exact same [`instrument_source`] entry point used by the
    /// preprocessor, so unit tests can assert on rewrites and probe metadata without spinning
    /// up a full solc compilation.
    fn instrument(source: &str) -> (String, Vec<InstrumentedCoverageProbe>) {
        let (content, probes, _) = instrument_with_edits(source);
        (content, probes)
    }

    /// Like [`instrument`], but also returns the edits that map the rewrite to `source`.
    fn instrument_with_edits(
        source: &str,
    ) -> (String, Vec<InstrumentedCoverageProbe>, Vec<SourceEdit>) {
        let version = Version::new(0, 8, 25);
        let path = PathBuf::from("Test.sol");
        let sess = Session::builder().with_buffer_emitter(Default::default()).build();
        let mut compiler = Compiler::new(sess);
        let result = compiler.enter_mut(|compiler| {
            let src_file = compiler
                .sess()
                .source_map()
                .new_source_file(FileName::from(path.clone()), source.to_owned())
                .expect("failed to create source file");
            let mut pcx = compiler.parse();
            pcx.add_file(src_file);
            pcx.parse();
            let gcx = compiler.gcx();
            let (_, ast_source) = gcx.get_ast_source(&path).expect("missing ast source");
            let ast = ast_source.ast.as_ref().expect("missing ast");
            instrument_source(&path, source, &version, &ast_source.file, ast)
        });
        if let Some(Err(diags)) = compiler.sess().emitted_errors() {
            panic!("input source failed to parse:\n{diags}");
        }
        result.expect("instrumentation produced no output")
    }

    /// Asserts that the instrumented source re-parses without errors, i.e. the rewrite is
    /// syntactically valid Solidity.
    #[track_caller]
    fn assert_reparses(instrumented: &str) {
        let sess = Session::builder().with_buffer_emitter(Default::default()).build();
        let mut compiler = Compiler::new(sess);
        compiler.enter_mut(|compiler| {
            let source_map = compiler.sess().source_map();
            // Provide the virtual coverage library so the injected import resolves.
            let lib_file = source_map
                .new_source_file(
                    FileName::from(PathBuf::from(COVERAGE_LIBRARY_PATH)),
                    COVERAGE_LIBRARY_SOURCE.to_owned(),
                )
                .expect("failed to create library source file");
            let src_file = source_map
                .new_source_file(
                    FileName::from(PathBuf::from("Reparse.sol")),
                    instrumented.to_owned(),
                )
                .expect("failed to create source file");
            let mut pcx = compiler.parse();
            pcx.add_file(lib_file);
            pcx.add_file(src_file);
            pcx.parse();
        });
        if let Some(Err(diags)) = compiler.sess().emitted_errors() {
            panic!("instrumented source failed to re-parse:\n{diags}\n---\n{instrumented}");
        }
    }

    fn count_kind(
        probes: &[InstrumentedCoverageProbe],
        pred: impl Fn(&InstrumentedCoverageProbeKind) -> bool,
    ) -> usize {
        probes.iter().filter(|p| pred(&p.kind)).count()
    }

    #[test]
    fn ignore_file_directives_only_match_comments() {
        for directive in IGNORE_FILE_DIRECTIVES {
            for literal in [format!(r#""{directive}""#), format!(r#""/* {directive} */""#)] {
                let source = format!(
                    "contract C {{ function f() external pure returns (string memory) {{ return {literal}; }} }}"
                );
                assert!(!has_ignore_file_directive(&source), "{source}");
                let (output, _) = instrument(&source);
                assert_reparses(&output);
            }
            assert!(has_ignore_file_directive(&format!("// {directive}\ncontract C {{}}")));
            assert!(has_ignore_file_directive(&format!(
                "/*\n * {directive}\n */\ncontract C {{}}"
            )));
        }
    }

    #[test]
    fn ignore_next_directives_only_match_comments() {
        for directive in IGNORE_NEXT_DIRECTIVES {
            let source = format!(
                r#"
contract C {{
    function f() external pure returns (string memory) {{
        string memory text = "/* {directive} */";
        return text;
    }}
}}
"#
            );
            let (output, probes) = instrument(&source);
            assert_reparses(&output);
            assert_eq!(
                count_kind(&probes, |kind| matches!(
                    kind,
                    InstrumentedCoverageProbeKind::Statement
                )),
                2
            );

            for comment in [
                format!("// {directive}\n"),
                format!("/* {directive} */"),
                format!("/*\n * {directive} */\n"),
                format!("/*\n * {directive}\n */\n"),
            ] {
                let source = format!(
                    r#"
contract C {{
    function f() external pure returns (uint256) {{
        {comment}return 1;
    }}
}}
"#
                );
                let (output, probes) = instrument(&source);
                assert_reparses(&output);
                assert_eq!(
                    count_kind(&probes, |kind| matches!(
                        kind,
                        InstrumentedCoverageProbeKind::Statement
                    )),
                    0,
                    "{source}"
                );
            }
        }
    }

    #[test]
    fn instruments_basic_statement() {
        let (out, probes) = instrument(
            r#"
contract C {
    function f(uint256 x) external pure returns (uint256) {
        return x + 1;
    }
}
"#,
        );
        assert_reparses(&out);
        // One function probe and one statement probe (the return).
        assert_eq!(
            count_kind(&probes, |k| matches!(k, InstrumentedCoverageProbeKind::Function { .. })),
            1
        );
        assert!(
            count_kind(&probes, |k| matches!(k, InstrumentedCoverageProbeKind::Statement)) >= 1
        );
    }

    #[test]
    fn edits_map_instrumented_offsets_to_original() {
        let source = r#"
contract C {
    function f(uint256 x) external pure returns (uint256) {
        if (x > 1) return x;
        require(x != 0, "zero");
        return x + 1;
    }
}
"#;
        let (out, _, edits) = instrument_with_edits(source);
        assert!(edits.len() > 1);
        assert!(edits.windows(2).all(|pair| pair[0].start <= pair[1].start));

        // Removing every insertion restores the original source.
        let mut restored = String::new();
        let mut cursor = 0;
        let mut shift = 0;
        for edit in &edits {
            assert_eq!(edit.start, edit.end, "instrumentation only inserts");
            let start = edit.start + shift;
            restored.push_str(&out[cursor..start]);
            cursor = start + edit.len;
            shift += edit.len;
        }
        restored.push_str(&out[cursor..]);
        assert_eq!(restored, source);

        // Original code maps back to its own offset; inserted probes map to their insertion point.
        for snippet in ["return x + 1", "require(x != 0", "uint256 x"] {
            let offset = out.find(snippet).unwrap();
            assert_eq!(SourceEdit::original_offset(&edits, offset), source.find(snippet).unwrap());
        }
        let probe = out.find("__FoundryCoverage.sendHit").unwrap();
        let original = SourceEdit::original_offset(&edits, probe);
        assert!(edits.iter().any(|edit| edit.start == original));
    }

    #[test]
    fn instruments_require_branch() {
        let (out, probes) = instrument(
            r#"
contract C {
    function f(uint256 x) external pure {
        require(x > 0, "nonzero");
    }
}
"#,
        );
        assert_reparses(&out);
        assert_eq!(
            count_kind(&probes, |k| matches!(k, InstrumentedCoverageProbeKind::RequirePre { .. })),
            1
        );
        assert_eq!(
            count_kind(&probes, |k| matches!(k, InstrumentedCoverageProbeKind::RequirePost { .. })),
            1
        );
    }

    #[test]
    fn require_with_logical_condition_instruments_both() {
        // A `require` whose condition is a logical expression keeps its pre/post branch *and*
        // instruments the inner `||`, and the combined rewrite is still valid Solidity.
        let (out, probes) = instrument(
            r#"
contract C {
    function f(bool a, bool b) external pure {
        require(a || b, "fail");
    }
}
"#,
        );
        assert_reparses(&out);
        assert_eq!(
            count_kind(&probes, |k| matches!(k, InstrumentedCoverageProbeKind::RequirePre { .. })),
            1
        );
        // The inner `||` contributes its two operand branch paths.
        assert_eq!(branch_paths(&probes), vec![(1, 0), (1, 1)]);
    }

    #[test]
    fn instruments_if_else_branches() {
        let (out, probes) = instrument(
            r#"
contract C {
    function f(bool b) external pure returns (uint256 r) {
        if (b) {
            r = 1;
        } else {
            r = 2;
        }
    }
}
"#,
        );
        assert_reparses(&out);
        assert_eq!(
            count_kind(&probes, |k| matches!(k, InstrumentedCoverageProbeKind::Branch { .. })),
            2
        );
    }

    /// Collects the `(branch_id, path_id)` of every branch probe.
    fn branch_paths(probes: &[InstrumentedCoverageProbe]) -> Vec<(u32, u32)> {
        let mut out = probes
            .iter()
            .filter_map(|p| match p.kind {
                InstrumentedCoverageProbeKind::Branch { branch_id, path_id } => {
                    Some((branch_id, path_id))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        out.sort_unstable();
        out
    }

    #[test]
    fn instruments_logical_or_branches() {
        let (out, probes) = instrument(
            r#"
contract C {
    function f(bool a, bool b) external pure returns (bool) {
        return a || b;
    }
}
"#,
        );
        assert_reparses(&out);
        // Two branch outcomes for the single `||`.
        let branches = branch_paths(&probes);
        assert_eq!(branches.len(), 2, "expected 2 branch probes, got {branches:?}");
        assert_eq!(branches, vec![(0, 0), (0, 1)]);
        // Operands wrapped with the boolean-returning helper.
        assert!(out.contains("sendHitReturn"), "expected sendHitReturn in:\n{out}");
    }

    #[test]
    fn instruments_logical_and_branches() {
        let (out, probes) = instrument(
            r#"
contract C {
    function f(bool a, bool b) external pure returns (bool) {
        return a && b;
    }
}
"#,
        );
        assert_reparses(&out);
        assert_eq!(branch_paths(&probes), vec![(0, 0), (0, 1)]);
    }

    #[test]
    fn instruments_ternary_branches() {
        let (out, probes) = instrument(
            r#"
contract C {
    function f(bool b) external pure returns (uint256) {
        return b ? 1 : 2;
    }
}
"#,
        );
        assert_reparses(&out);
        assert_eq!(branch_paths(&probes), vec![(0, 0), (0, 1)]);
    }

    #[test]
    fn instruments_nested_logical_branches() {
        let (out, probes) = instrument(
            r#"
contract C {
    function f(bool a, bool b, bool c) external pure returns (bool) {
        return a || (b && c);
    }
}
"#,
        );
        assert_reparses(&out);
        // Two logical operators => two branches, two paths each.
        assert_eq!(branch_paths(&probes), vec![(0, 0), (0, 1), (1, 0), (1, 1)]);
    }

    #[test]
    fn assert_is_not_a_branch() {
        let (out, probes) = instrument(
            r#"
contract C {
    function f(uint256 x) external pure {
        assert(x > 0);
    }
}
"#,
        );
        assert_reparses(&out);
        // assert() is a statement, never a branch (matches Forge).
        assert_eq!(
            count_kind(&probes, |k| matches!(k, InstrumentedCoverageProbeKind::Branch { .. })),
            0
        );
    }

    #[test]
    fn bare_if_gets_implicit_else_branch() {
        let (out, probes) = instrument(
            r#"
contract C {
    function f(bool b) external pure returns (uint256 r) {
        if (b) {
            r = 1;
        }
    }
}
"#,
        );
        assert_reparses(&out);
        // A bare `if` still yields two branch outcomes: then (path 0) and the synthesized
        // else (path 1).
        assert_eq!(branch_paths(&probes), vec![(0, 0), (0, 1)]);
        assert!(out.contains(" else {"), "expected synthesized else in:\n{out}");
    }

    #[test]
    fn nested_bare_if_does_not_create_dangling_else() {
        // The inner `if` is a single-statement then-body of the outer `if`; both are bare.
        // Each gets its own implicit else without producing `} else {} else {}`.
        let (out, probes) = instrument(
            r#"
contract C {
    function f(bool a, bool b) external pure returns (uint256 r) {
        if (a)
            if (b)
                r = 1;
    }
}
"#,
        );
        assert_reparses(&out);
        // Two `if`s => two branches, two paths each.
        assert_eq!(branch_paths(&probes), vec![(0, 0), (0, 1), (1, 0), (1, 1)]);
    }

    #[test]
    fn modifier_require_guard_is_a_branch() {
        let (out, probes) = instrument(
            r#"
contract C {
    modifier onlyPositive(uint256 x) {
        require(x > 0, "nonpositive");
        _;
    }

    function f(uint256 x) external onlyPositive(x) returns (uint256) {
        return x;
    }
}
"#,
        );
        assert_reparses(&out);
        // The modifier body is instrumented like any function body, so its `require` guard
        // contributes require pre/post branch probes.
        assert_eq!(
            count_kind(&probes, |k| matches!(k, InstrumentedCoverageProbeKind::RequirePre { .. })),
            1
        );
        // The modifier itself and the function are both recorded as functions.
        assert_eq!(
            count_kind(&probes, |k| matches!(k, InstrumentedCoverageProbeKind::Function { .. })),
            2
        );
    }

    #[test]
    fn header_expressions_are_not_instrumented_as_branches() {
        // A logical expression in a modifier invocation argument is part of the function header,
        // not its body, and must not be counted as a branch.
        let (out, probes) = instrument(
            r#"
contract C {
    modifier m(bool ok) { require(ok); _; }
    function f(bool a, bool b) external m(a || b) returns (uint256) {
        return 1;
    }
}
"#,
        );
        assert_reparses(&out);
        // Only the modifier's own `require` branch; no branch from the `a || b` in `m(a || b)`.
        assert_eq!(
            count_kind(&probes, |k| matches!(k, InstrumentedCoverageProbeKind::Branch { .. })),
            0
        );
        assert!(!out.contains("sendHitReturn"), "header expression must not be wrapped:\n{out}");
    }

    #[test]
    fn empty_if_body_produces_no_branch() {
        // `if (b) {}` with empty bodies must not produce phantom branches (matches the native
        // analyzer), but a non-empty body still does.
        let (out, probes) = instrument(
            r#"
contract C {
    function f(bool b) external pure returns (uint256 r) {
        if (b) {}
        if (b) { r = 1; }
    }
}
"#,
        );
        assert_reparses(&out);
        // Only the second `if` (non-empty) contributes branches: then + implicit else = 2.
        assert_eq!(
            count_kind(&probes, |k| matches!(k, InstrumentedCoverageProbeKind::Branch { .. })),
            2
        );
    }

    #[test]
    fn braceless_require_body_reparses_for_every_control_form() {
        // A `require` as the brace-less body of a control statement must wrap into a block with
        // the require probes *inside* it, so the braces stay attached and the construct still
        // compiles (regression: the synthesized braces used to detach, dangling any `else`).
        let cases = [
            "if (a) require(x > 0, \"e\");",
            "if (a) require(x > 0, \"e\"); else { x = 1; }",
            "if (a) {} else require(x > 0, \"e\");",
            "while (x > 0) require(x > 0, \"e\");",
            "for (uint256 i; i < x; i++) require(x > 0, \"e\");",
            "do require(x > 0, \"e\"); while (x > 0);",
        ];
        for body in cases {
            let src = format!(
                "contract C {{ function f(bool a, uint256 x) external pure {{ {body} }} }}"
            );
            let (out, probes) = instrument(&src);
            assert_reparses(&out);
            // The require's pre/post branch probes are still recorded.
            assert_eq!(
                count_kind(&probes, |k| matches!(
                    k,
                    InstrumentedCoverageProbeKind::RequirePre { .. }
                )),
                1,
                "missing require branch for: {body}"
            );
        }
    }

    #[test]
    fn ignored_control_bodies_do_not_add_expression_or_implicit_else_probes() {
        for statement in ["return a ? 1 : 2;", "require(a || b);"] {
            for body in [statement.to_string(), format!("{{ {statement} }}")] {
                let source = format!(
                    "contract C {{\nfunction f(bool a, bool b) external pure returns (uint256 x) {{\nif (a)\n/* forge coverage ignore next */ {body}\n}}\n}}"
                );
                let (output, probes) = instrument(&source);
                assert_reparses(&output);
                assert_eq!(probes.len(), 1, "{source}: {probes:?}");
                assert!(matches!(probes[0].kind, InstrumentedCoverageProbeKind::Function { .. }));
            }
        }
    }

    #[test]
    fn braceless_bodies_preserve_expression_branches() {
        for statement in ["return a ? 1 : 2;", "require(a || b);", "x = a && b ? 1 : 2;"] {
            let source = |body: &str| {
                format!(
                    "contract C {{ function f(bool a, bool b) external pure returns (uint256 x) {{ if (a) {body} }} }}"
                )
            };
            let (bare, bare_probes) = instrument(&source(statement));
            let (braced, braced_probes) = instrument(&source(&format!("{{ {statement} }}")));
            assert_reparses(&bare);
            assert_reparses(&braced);
            assert_eq!(branch_paths(&bare_probes), branch_paths(&braced_probes), "{statement}");
        }
    }

    #[test]
    fn instruments_yul_statements() {
        // A non-`pure` function: the Yul probe's `staticcall` is permitted here.
        let (out, probes) = instrument(
            r#"
contract C {
    function f(uint256 x) external returns (uint256 r) {
        assembly {
            let a := add(x, 5)
            let b := mul(a, 2)
            r := add(a, b)
        }
    }
}
"#,
        );
        assert_reparses(&out);
        // The assembly block is one statement, plus one per Yul statement inside it (3).
        assert!(
            count_kind(&probes, |k| matches!(k, InstrumentedCoverageProbeKind::Statement)) >= 4,
            "expected the assembly block plus its 3 inner statements, got probes: {probes:?}"
        );
        assert!(out.contains("staticcall"), "expected a yul probe in:\n{out}");
    }

    #[test]
    fn instruments_yul_if_branch() {
        let (out, probes) = instrument(
            r#"
contract C {
    function f(uint256 x) external returns (uint256 r) {
        assembly {
            if gt(x, 10) {
                r := 1
            }
        }
    }
}
"#,
        );
        assert_reparses(&out);
        // A Yul `if` is a one-sided branch.
        assert_eq!(
            count_kind(&probes, |k| matches!(k, InstrumentedCoverageProbeKind::Branch { .. })),
            1
        );
    }

    #[test]
    fn instruments_yul_switch_branches() {
        let (out, probes) = instrument(
            r#"
contract C {
    function f(uint256 x) external returns (uint256 r) {
        assembly {
            switch x
            case 0 { r := 10 }
            case 1 { r := 20 }
            default { r := 30 }
        }
    }
}
"#,
        );
        assert_reparses(&out);
        // Each case body (including default) is a branch path: 3 total.
        assert_eq!(
            count_kind(&probes, |k| matches!(k, InstrumentedCoverageProbeKind::Branch { .. })),
            3
        );
    }

    #[test]
    fn instruments_yul_for_loop() {
        let (out, probes) = instrument(
            r#"
contract C {
    function f(uint256 n) external returns (uint256 sum) {
        assembly {
            for { let i := 0 } lt(i, n) { i := add(i, 1) } {
                sum := add(sum, i)
            }
        }
    }
}
"#,
        );
        assert_reparses(&out);
        // The for body plus the init (`let i`), step (`i := ...`), and body (`sum := ...`)
        // statements are all instrumented.
        assert!(
            count_kind(&probes, |k| matches!(k, InstrumentedCoverageProbeKind::Statement)) >= 4,
            "expected for-loop yul statements instrumented, got probes: {probes:?}"
        );
    }

    #[test]
    fn pure_function_assembly_is_not_yul_instrumented() {
        // A `pure` function cannot host the Yul probe's `staticcall`, so its assembly stays a
        // single statement and no Yul-level `staticcall` probe is injected.
        let (out, probes) = instrument(
            r#"
contract C {
    function f(uint256 x) external pure returns (uint256 r) {
        assembly {
            let a := add(x, 5)
            r := mul(a, 2)
        }
    }
}
"#,
        );
        assert_reparses(&out);
        assert!(
            !out.contains("staticcall"),
            "pure function assembly must not get a yul staticcall probe:\n{out}"
        );
        // Exactly the function probe, the assembly-block statement, and its line.
        assert_eq!(
            count_kind(&probes, |k| matches!(k, InstrumentedCoverageProbeKind::Statement)),
            1
        );
    }

    #[test]
    fn type_expressions_with_ternaries_are_not_instrumented() {
        let (out, probes) = instrument(
            r#"
contract C {
    bool constant FLAG = true;
    function f(bytes calldata data) external pure returns (uint256) {
        uint256[FLAG ? 2 : 4] memory arr;
        uint256[FLAG && true ? 1 : 2] memory decoded = abi.decode(data, (uint256[FLAG && true ? 1 : 2]));
        return arr.length + decoded.length;
    }
}
"#,
        );
        assert_reparses(&out);
        assert_eq!(
            count_kind(&probes, |k| matches!(k, InstrumentedCoverageProbeKind::Branch { .. })),
            0,
            "constant type expressions must not receive branch probes:\n{out}"
        );
    }

    #[test]
    fn for_loop_init_expressions_are_instrumented_without_statement_probe() {
        let (out, probes) = instrument(
            r#"
contract C {
    function f(bool flag) external pure returns (uint256 sum) {
        for (uint256 i = flag ? 1 : 0; i < 2; i++) {
            sum += i;
        }
    }
}
"#,
        );
        assert_reparses(&out);
        assert_eq!(branch_paths(&probes), vec![(0, 0), (0, 1)]);
    }

    #[test]
    fn ignore_next_does_not_bleed_to_sibling_or_miss_try_catch() {
        let (out, probes) = instrument(
            r#"
interface ICallee {
    function ping() external pure;
}

contract C {
    function f(ICallee callee) external pure returns (uint256 x) {
        /* forge coverage ignore next */ x = 1; x = 2;
        try callee.ping() {
            x = 3;
        }
        // forge coverage ignore next
        catch {
            x = 4;
        }
    }
}
"#,
        );
        assert_reparses(&out);
        // `x = 1;` is ignored, `x = 2;` and `x = 3;` are instrumented, `catch { x = 4; }` is
        // ignored.
        assert_eq!(
            count_kind(&probes, |k| matches!(k, InstrumentedCoverageProbeKind::Statement)),
            2,
            "unexpected statement probes:\n{out}\n{probes:?}"
        );
        assert_eq!(branch_paths(&probes), vec![(0, 0)]);
    }

    #[test]
    fn ignore_next_applies_to_yul_switch_cases() {
        let (out, probes) = instrument(
            r#"
contract C {
    function f(uint256 x) external returns (uint256 r) {
        assembly {
            switch x
            case 0 { r := 1 }
            // forge coverage ignore next
            default { r := 2 }
        }
    }
}
"#,
        );
        assert_reparses(&out);
        assert_eq!(branch_paths(&probes), vec![(0, 0)]);
        // Assembly block + `r := 1` (the ignored `default` case and `r := 2` are excluded).
        assert_eq!(
            count_kind(&probes, |k| matches!(k, InstrumentedCoverageProbeKind::Statement)),
            2,
            "unexpected statement probes:\n{out}\n{probes:?}"
        );
    }
}
