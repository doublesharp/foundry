use alloy_primitives::{B256, keccak256};
use eyre::Result;
use foundry_common::errors::convert_solar_errors;
use foundry_compilers::{
    Compiler, ProjectPathsConfig, SourceParser,
    artifacts::{SolcLanguage, Source},
    error,
    multi::{MultiCompiler, MultiCompilerInput, MultiCompilerLanguage},
    project::Preprocessor,
    solc::{SolcCompiler, SolcVersionedInput},
};
use semver::Version;
use solar::{
    ast::{
        self, BinOpKind, ExprKind, FunctionKind, ItemKind, StateMutability, StmtKind, Visit, yul,
    },
    data_structures::Never,
    interface::{BytePos, Span},
    parse::interface::SourceMap,
};
use std::{
    collections::{BTreeMap, HashSet},
    ops::{ControlFlow, Range},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

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
            let ptr := mload(0x40)
            mstore(ptr, tag)
            pop(staticcall(gas(), coverageAddress, ptr, 0x20, 0, 0))
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
}

impl CoverageInstrumentationPreprocessor {
    pub const fn new(
        metadata: InstrumentedCoverageMetadataRef,
        include_libs: bool,
        exclude_tests: bool,
        excluded_sources: Option<regex::Regex>,
    ) -> Self {
        Self { metadata, include_libs, exclude_tests, excluded_sources }
    }

    fn preprocess_solc(
        &self,
        _solc: &SolcCompiler,
        input: &mut SolcVersionedInput,
        paths: &ProjectPathsConfig<SolcLanguage>,
        _mocks: &mut HashSet<PathBuf>,
    ) -> error::Result<()> {
        let mut instrumented_sources = BTreeMap::new();

        let mut compiler =
            foundry_compilers::resolver::parse::SolParser::new(paths.with_language_ref())
                .into_compiler();
        let _ = compiler.enter_mut(|compiler| -> solar::interface::Result {
            let mut pcx = compiler.parse();
            let mut candidate_paths = Vec::new();

            for (path, source) in &input.input.sources {
                if let Ok(src_file) = compiler
                    .sess()
                    .source_map()
                    .new_source_file(path.clone(), source.content.as_str().to_owned())
                    && self.should_instrument(paths, path)
                {
                    pcx.add_file(src_file);
                    candidate_paths.push(path.clone());
                }
            }

            pcx.parse();
            let gcx = compiler.gcx();
            for path in candidate_paths {
                let Some(source) = input.input.sources.get(&path) else { continue };
                let Some((_, ast_source)) = gcx.get_ast_source(&path) else { continue };
                let Some(ast) = ast_source.ast.as_ref() else { continue };
                let Some((content, source_probes)) = instrument_source(
                    &path,
                    source.content.as_str(),
                    &input.version,
                    gcx.sess.source_map(),
                    ast,
                ) else {
                    continue;
                };
                instrumented_sources.insert(path, (content, source_probes));
            }

            Ok(())
        });

        if let Err(err) = convert_solar_errors(compiler.dcx()) {
            warn!(%err, "failed coverage instrumentation");
            return Ok(());
        }

        let mut probes = Vec::new();
        let mut changed = false;

        for (path, (content, mut source_probes)) in instrumented_sources {
            let Some(source) = input.input.sources.get_mut(&path) else { continue };
            source.content = Arc::new(content);
            changed = true;
            probes.append(&mut source_probes);
        }

        if changed {
            input
                .input
                .sources
                .insert(PathBuf::from(COVERAGE_LIBRARY_PATH), Source::new(COVERAGE_LIBRARY_SOURCE));
        }

        self.metadata.lock().expect("coverage metadata lock poisoned").probes.extend(probes);
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
        if let Some(excluded_sources) = &self.excluded_sources {
            let relative_path = path.strip_prefix(&paths.root).unwrap_or(path);
            if excluded_sources.is_match(&relative_path.to_string_lossy()) {
                return false;
            }
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
    source_map: &SourceMap,
    ast: &'ast ast::SourceUnit<'ast>,
) -> Option<(String, Vec<InstrumentedCoverageProbe>)> {
    let mut collector = StatementCollector::new(path, source, version, source_map);
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
    for (_, update) in updates {
        content.push_str(&source[cursor..update.range.start]);
        content.push_str(&update.text);
        cursor = update.range.end;
    }
    content.push_str(&source[cursor..]);
    content.push('\n');
    content.push_str(COVERAGE_IMPORT);
    content.push('\n');
    Some((content, collector.probes))
}

/// Sort rank for an [`Edge`] at a shared byte offset: closes first, then points, then opens.
const fn edge_rank(edge: Edge) -> u8 {
    match edge {
        Edge::Close => 0,
        Edge::Point => 1,
        Edge::Open => 2,
    }
}

#[derive(Debug)]
struct StatementCollector<'a> {
    path: &'a Path,
    source: &'a str,
    version: &'a Version,
    source_map: &'a SourceMap,
    updates: Vec<SourceUpdate>,
    probes: Vec<InstrumentedCoverageProbe>,
    next_probe: u64,
    next_branch: u32,
    contract_name: Box<str>,
    probe_mode: ProbeMode,
}

impl<'a> StatementCollector<'a> {
    fn new(
        path: &'a Path,
        source: &'a str,
        version: &'a Version,
        source_map: &'a SourceMap,
    ) -> Self {
        Self {
            path,
            source,
            version,
            source_map,
            updates: Vec::new(),
            probes: Vec::new(),
            next_probe: 0,
            next_branch: 0,
            contract_name: "".into(),
            probe_mode: ProbeMode::Direct,
        }
    }

    fn push_probe(&mut self, span: Span) {
        self.push_probe_kind(InstrumentedCoverageProbeKind::Statement, span);
    }

    fn push_require_branch(&mut self, stmt: &'_ ast::Stmt<'_>) {
        let range = self.trim_statement_span(stmt.span);
        if range.is_empty() {
            return;
        }

        let full_range = self.source_map.span_to_source(stmt.span).unwrap().data;
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
        if range.is_empty() {
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
        if range.is_empty() {
            return;
        }
        let full_range = self.source_map.span_to_source(stmt.span).unwrap().data;
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

    fn push_block_entry_probe(
        &mut self,
        block: &ast::Block<'_>,
        kind: InstrumentedCoverageProbeKind,
    ) {
        let range = self.trim_statement_span(block.span);
        if range.is_empty() {
            return;
        }
        let full_range = self.source_map.span_to_source(block.span).unwrap().data;
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
        let range = self.source_map.span_to_source(span).unwrap().data;
        if range.is_empty() {
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
        let range = self.source_map.span_to_source(block.span).unwrap().data;
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
        self.next_probe += 1;
    }

    fn probe_text(&self, tag: B256) -> String {
        match self.probe_mode {
            ProbeMode::Direct => format!(
                "assembly (\"memory-safe\") {{ mstore(0x00, {tag}) pop(staticcall(gas(), 0xc0bEc0BEc0BeC0bEC0beC0bEC0bEC0beC0beC0BE, 0x00, 0x20, 0, 0)) }}\n"
            ),
            ProbeMode::Library => format!("__FoundryCoverage.sendHit({tag}); "),
        }
    }

    /// A coverage probe emitted *inside* an `assembly` block, as a self-contained Yul block.
    ///
    /// The scratch slot `0x00` is saved into a uniquely-named local and restored afterwards, so
    /// the surrounding (memory-safe) assembly is unaffected; the probe writes the tag there only
    /// long enough to forward it to the sentinel via `staticcall`. The wrapping `{ }` scopes the
    /// temporary so it cannot collide with user variables or leak into later statements.
    fn yul_probe_text(&self, tag: B256) -> String {
        let slot = self.next_probe;
        format!(
            "{{ let _cov{slot} := mload(0x00) mstore(0x00, {tag}) pop(staticcall(gas(), 0xc0bEc0BEc0BeC0bEC0beC0bEC0bEC0beC0beC0BE, 0x00, 0x20, 0x00, 0x00)) mstore(0x00, _cov{slot}) }} "
        )
    }

    fn trim_statement_span(&self, mut span: Span) -> Range<usize> {
        if let Ok(snippet) = self.source_map.span_to_snippet(span)
            && let Some(stripped) = snippet.strip_suffix(';')
        {
            let stripped = stripped.trim_end();
            let skipped = snippet.len() - stripped.len();
            span = span.with_hi(span.hi() - BytePos::from_usize(skipped));
        }
        self.source_map.span_to_source(span).unwrap().data
    }

    fn line_range(&self, span: Span) -> Range<u32> {
        let lines = self.source_map.span_to_lines(span).unwrap().data;
        let first = lines.first().unwrap();
        let last = lines.last().unwrap();
        first.line_index as u32 + 1..last.line_index as u32 + 2
    }

    fn make_tag(&self, start: usize) -> B256 {
        let payload = format!(
            "foundry-coverage:{}:{}:{}:{}",
            self.version,
            self.path.display(),
            start,
            self.next_probe
        );
        keccak256(payload)
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
        match &stmt.kind {
            StmtKind::Block(block) | StmtKind::UncheckedBlock(block) => {
                self.push_block_entry_probe(block, kind);
                if let Some(trailing) = trailing {
                    let end = self.source_map.span_to_source(block.span).unwrap().data.end;
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
            _ => {
                self.wrap_and_probe(stmt, kind, trailing);
                if Self::is_require_stmt(stmt) {
                    self.visit_stmt(stmt)?;
                }
                ControlFlow::Continue(())
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
        self.source_map.span_to_source(span).unwrap().data
    }

    /// Wraps the boolean sub-expression at `span` in a `sendHitReturn(tag, (..))` call so the
    /// probe fires exactly when the sub-expression is evaluated, while passing its value through
    /// unchanged. Records a [`Branch`] probe for `(branch_id, path_id)`.
    ///
    /// [`Branch`]: InstrumentedCoverageProbeKind::Branch
    fn wrap_bool_branch(&mut self, span: Span, branch_id: u32, path_id: u32) {
        let range = self.source_range(span);
        if range.is_empty() {
            return;
        }
        let tag = self.make_tag(range.start);
        self.updates.push(SourceUpdate::open(
            range.start,
            format!("__FoundryCoverage.sendHitReturn({tag}, ("),
        ));
        self.updates.push(SourceUpdate::close(range.end, "))"));
        self.record_probe(
            InstrumentedCoverageProbeKind::Branch { branch_id, path_id },
            span,
            range,
            tag,
        );
    }

    /// Instruments a logical `&&`/`||` expression so each operand becomes a branch outcome,
    /// then recurses so nested logical/ternary operands are also instrumented.
    fn instrument_logical_branch<'ast>(
        &mut self,
        lhs: &'ast ast::Expr<'ast>,
        rhs: &'ast ast::Expr<'ast>,
    ) -> ControlFlow<<Self as ast::Visit<'ast>>::BreakValue> {
        let branch_id = self.next_branch_id();
        // Path 0 is the always-evaluated left operand; path 1 is the right operand, which is
        // only evaluated when the left operand does not short-circuit the expression.
        self.wrap_bool_branch(lhs.span, branch_id, 0);
        self.wrap_bool_branch(rhs.span, branch_id, 1);
        self.visit_expr(lhs)?;
        self.visit_expr(rhs)
    }

    /// Instruments a ternary `cond ? a : b` so the condition reports both outcomes, then
    /// recurses into the condition and both arms.
    ///
    /// The arms may be of any type, so they are left untouched. Instead the condition is
    /// rewritten into a short-circuit pair that preserves its truth value:
    /// `(sendHitReturn(tag0, (cond)) || sendHitReturn(tag1, false))`. When `cond` is true the
    /// first call fires `tag0` and the OR short-circuits; when `cond` is false the second call
    /// fires `tag1`. The resulting boolean equals `cond`.
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
            self.updates.push(SourceUpdate::open(
                range.start,
                format!("(__FoundryCoverage.sendHitReturn({true_tag}, ("),
            ));
            self.updates.push(SourceUpdate::close(
                range.end,
                format!(")) || __FoundryCoverage.sendHitReturn({false_tag}, false))"),
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
                }
                self.walk_item(item)?;
                self.probe_mode = previous_probe_mode;
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }

    fn visit_stmt(&mut self, stmt: &'ast ast::Stmt<'ast>) -> ControlFlow<Self::BreakValue> {
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
                let branch_id = self.next_branch_id();
                // The implicit else (`path_id: 1`) for a bare `if` is fused onto the then-body's
                // closing brace as a single insertion, so it binds to this `if` and orders
                // correctly relative to enclosing scopes even for nested brace-less `if`s.
                let implicit_else = else_stmt.is_none().then(|| {
                    let then_range = self.source_map.span_to_source(then_stmt.span).unwrap().data;
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
                let _ = init;
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

    fn visit_expr(&mut self, expr: &'ast ast::Expr<'ast>) -> ControlFlow<Self::BreakValue> {
        match &expr.kind {
            ExprKind::Binary(lhs, op, rhs) if matches!(op.kind, BinOpKind::And | BinOpKind::Or) => {
                self.instrument_logical_branch(lhs, rhs)
            }
            ExprKind::Ternary(cond, then_expr, else_expr) => {
                self.instrument_ternary_branch(cond, then_expr, else_expr)
            }
            // Not a branch on its own; descend so any nested logical/ternary operands are found.
            _ => self.walk_expr(expr),
        }
    }

    fn visit_yul_stmt(&mut self, stmt: &'ast yul::Stmt<'ast>) -> ControlFlow<Self::BreakValue> {
        use yul::StmtKind as Yul;
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
    Ok(metadata.lock().expect("coverage metadata lock poisoned").clone())
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
            instrument_source(&path, source, &version, gcx.sess.source_map(), ast)
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
}
