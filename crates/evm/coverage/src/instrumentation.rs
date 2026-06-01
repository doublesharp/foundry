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
    ast::{self, FunctionKind, ItemKind, StateMutability, StmtKind, Visit},
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
    pub fn new(
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

            for (path, source) in input.input.sources.iter() {
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
        if !path.extension().and_then(|s| s.to_str()).is_some_and(|ext| ext == "sol") {
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
    updates.sort_by_key(|(index, update)| (update.range.start, *index));
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
        self.updates
            .push(SourceUpdate { range: range.start..range.start, text: self.probe_text(pre_tag) });
        self.record_probe(
            InstrumentedCoverageProbeKind::RequirePre { branch_id },
            stmt.span,
            range.clone(),
            pre_tag,
        );

        let post_tag = self.make_tag(range.end);
        self.updates.push(SourceUpdate {
            range: full_range.end..full_range.end,
            text: self.probe_text(post_tag),
        });
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
        self.updates.push(SourceUpdate { range: range.start..range.start, text });
        self.record_probe(kind, span, range, tag);
    }

    fn wrap_and_probe(&mut self, stmt: &'_ ast::Stmt<'_>, kind: InstrumentedCoverageProbeKind) {
        let range = self.trim_statement_span(stmt.span);
        if range.is_empty() {
            return;
        }
        let full_range = self.source_map.span_to_source(stmt.span).unwrap().data;
        let tag = self.make_tag(range.start);
        self.updates.push(SourceUpdate {
            range: full_range.start..full_range.start,
            text: format!("{{ {}", self.probe_text(tag)),
        });
        self.updates
            .push(SourceUpdate { range: full_range.end..full_range.end, text: " }".into() });
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
        self.updates.push(SourceUpdate { range: insert_at..insert_at, text: self.probe_text(tag) });
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
        match &stmt.kind {
            StmtKind::Block(block) | StmtKind::UncheckedBlock(block) => {
                self.push_block_entry_probe(block, kind);
                self.visit_stmt(stmt)
            }
            StmtKind::DoWhile(..)
            | StmtKind::For { .. }
            | StmtKind::If(..)
            | StmtKind::Try(_)
            | StmtKind::While(..) => {
                self.wrap_and_probe(stmt, kind);
                self.visit_stmt(stmt)
            }
            _ => {
                self.wrap_and_probe(stmt, kind);
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

    fn should_instrument_function(func: &ast::ItemFunction<'_>) -> bool {
        !matches!(func.kind, FunctionKind::Receive) && func.body.is_some()
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
            StmtKind::Break | StmtKind::Assembly(_) | StmtKind::Continue => {
                self.push_probe(stmt.span);
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
                self.visit_control_body(
                    then_stmt,
                    InstrumentedCoverageProbeKind::Branch { branch_id, path_id: 0 },
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
                self.visit_expr(&try_.expr)?;
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
}
