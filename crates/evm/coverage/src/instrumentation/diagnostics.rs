//! Maps compiler diagnostics on instrumented sources back to the original sources.

use super::{COVERAGE_LIBRARY_PATH, InstrumentedCoverageMetadata, InstrumentedSource};
use foundry_compilers::{
    artifacts::{Error, Severity},
    buildinfo::SourceEdit,
};
use solar::interface::{
    BytePos, ColorChoice, Span,
    diagnostics::{Diag, DiagId, Emitter, HumanBufferEmitter, Level},
    source_map::SourceMap,
};
use std::{path::Path, sync::Arc};

/// Marker of a location line in Solar's rendered diagnostics.
const SOLAR_LOCATION_MARKER: &str = "╭▸";

/// Marker of a location line in solc's rendered diagnostics.
const SOLC_LOCATION_MARKER: &str = "--> ";

/// Longest snippet line solc prints before truncating it.
const SOLC_MAX_LINE_LENGTH: usize = 150;

/// Context solc keeps on each side of a truncated snippet.
const SOLC_TRUNCATION_CONTEXT: usize = 35;

/// A diagnostic location translated to an original source.
struct OriginalLocation<'a> {
    file: &'a str,
    source: &'a InstrumentedSource,
    start: usize,
    end: usize,
}

impl InstrumentedCoverageMetadata {
    /// Maps a compiler diagnostic on instrumented sources back to the original sources.
    ///
    /// Updates the diagnostic's source locations and re-renders its snippets from the original
    /// source text, so reported lines, columns, and code match the user's files. Returns `false`
    /// for a warning or info that only concerns inserted coverage probes or the coverage library;
    /// such diagnostics describe the instrumentation rather than user code.
    pub fn remap_diagnostic(&self, error: &mut Error) -> bool {
        let Some(primary) = &error.source_location else { return true };
        if primary.file == COVERAGE_LIBRARY_PATH {
            return error.is_error();
        }
        let Some(primary) = self.original_location(&primary.file, primary.start, primary.end)
        else {
            return true;
        };
        if primary.start == primary.end
            && error.source_location.as_ref().is_some_and(|location| location.start < location.end)
            && !error.is_error()
        {
            return false;
        }

        let secondary = error
            .secondary_source_locations
            .iter()
            .map(|location| {
                self.original_location(location.file.as_deref()?, location.start?, location.end?)
            })
            .collect::<Vec<_>>();
        let edited = std::iter::once(&primary)
            .chain(secondary.iter().flatten())
            .any(|location| !location.source.edits.is_empty());
        if !edited {
            return true;
        }

        if let Some(formatted) = &error.formatted_message {
            let rendered = if formatted.contains(SOLAR_LOCATION_MARKER) {
                render_solar(error, &primary, &secondary)
            } else if formatted.contains(SOLC_LOCATION_MARKER) {
                replace_solc_locations(formatted, &primary, &secondary)
            } else {
                None
            };
            if let Some(rendered) = rendered {
                error.formatted_message = Some(rendered);
            }
        }

        if let Some(location) = &mut error.source_location {
            location.start = primary.start as i32;
            location.end = primary.end as i32;
        }
        for (location, original) in error.secondary_source_locations.iter_mut().zip(&secondary) {
            if let Some(original) = original {
                location.start = Some(original.start as i32);
                location.end = Some(original.end as i32);
            }
        }
        true
    }

    /// Translates an instrumented byte range of `file` to its original source.
    fn original_location(&self, file: &str, start: i32, end: i32) -> Option<OriginalLocation<'_>> {
        let (path, source) = self.sources.get_key_value(Path::new(file))?;
        let file = path.to_str()?;
        let start = usize::try_from(start).ok()?;
        let end = usize::try_from(end).ok()?;
        (start <= end).then(|| OriginalLocation {
            file,
            source,
            start: SourceEdit::original_offset(&source.edits, start),
            end: SourceEdit::original_offset(&source.edits, end),
        })
    }
}

/// Re-renders a Solar diagnostic from the original sources with Solar's own emitter.
fn render_solar(
    error: &Error,
    primary: &OriginalLocation<'_>,
    secondary: &[Option<OriginalLocation<'_>>],
) -> Option<String> {
    let source_map = Arc::new(SourceMap::empty());
    let span = |location: &OriginalLocation<'_>| -> Option<Span> {
        let file = source_map
            .new_source_file_shared(Path::new(location.file), location.source.original.clone())
            .ok()?;
        Some(Span::new(
            file.start_pos + BytePos::from_usize(location.start),
            file.start_pos + BytePos::from_usize(location.end),
        ))
    };

    let level = match error.severity {
        Severity::Error => Level::Error,
        Severity::Warning => Level::Warning,
        Severity::Info => Level::Note,
    };
    let mut diag = Diag::new(level, error.message.clone());
    if let Some(code) = error.error_code {
        diag.code(DiagId::new_str(code.to_string()));
    }
    diag.span(span(primary)?);
    for (location, original) in error.secondary_source_locations.iter().zip(secondary) {
        let message = location.message.clone().unwrap_or_default();
        // Solar reports a child diagnostic without a span at the start of the primary file.
        match original {
            Some(original) if original.end > 0 => diag.span_note(span(original)?, message),
            _ => diag.note(message),
        };
    }

    let mut emitter = HumanBufferEmitter::new(ColorChoice::Never).source_map(Some(source_map));
    emitter.emit_diagnostic_ref(&diag);
    Some(std::mem::take(emitter.buffer_mut()))
}

/// Replaces the source location blocks of a solc diagnostic with ones rendered from the original
/// sources.
///
/// solc renders the primary location, then each secondary location, as a ` --> file:line:col:`
/// line followed by `|`-prefixed snippet lines. Returns `None` if the blocks do not correspond to
/// the diagnostic's locations.
fn replace_solc_locations(
    formatted: &str,
    primary: &OriginalLocation<'_>,
    secondary: &[Option<OriginalLocation<'_>>],
) -> Option<String> {
    let locations = std::iter::once(Some(primary))
        .chain(secondary.iter().map(Option::as_ref))
        .collect::<Vec<_>>();
    let mut rendered = String::with_capacity(formatted.len());
    let mut lines = formatted.split_inclusive('\n').peekable();
    let mut blocks = 0;
    while let Some(line) = lines.next() {
        let Some(header) = line.trim_start_matches(' ').strip_prefix(SOLC_LOCATION_MARKER) else {
            rendered.push_str(line);
            continue;
        };
        let mut block = line.to_string();
        while let Some(line) = lines.next_if(|line| is_solc_gutter_line(line)) {
            block.push_str(line);
        }
        match locations.get(blocks)? {
            Some(location) => {
                header.strip_prefix(location.file)?.strip_prefix(':')?;
                rendered.push_str(&render_solc_location(location));
            }
            None => rendered.push_str(&block),
        }
        blocks += 1;
    }
    (blocks == locations.len()).then_some(rendered)
}

/// Returns whether `line` is part of a solc snippet, e.g. `  |` or `12 |     code`.
fn is_solc_gutter_line(line: &str) -> bool {
    line.trim_start_matches(' ')
        .trim_start_matches(|c: char| c.is_ascii_digit())
        .trim_start_matches(' ')
        .starts_with('|')
}

/// Renders a source location like solc's `SourceReferenceFormatter`.
///
/// Columns and snippet truncation are byte-based, as in solc.
fn render_solc_location(location: &OriginalLocation<'_>) -> String {
    let source = location.source.original.as_bytes();
    let (line_number, start_column) = line_column(source, location.start);
    let (end_line, end_column) = line_column(source, location.end);
    let multiline = line_number != end_line;
    let mut text = line_at(source, location.start).to_vec();
    let mut snippet_start = start_column;
    let mut snippet_end = end_column;
    let mut length = if multiline {
        text.len().saturating_sub(start_column)
    } else {
        end_column.saturating_sub(start_column)
    };

    if length > SOLC_MAX_LINE_LENGTH {
        let lhs = (snippet_start + SOLC_TRUNCATION_CONTEXT).min(text.len());
        let rhs = (if multiline { text.len() } else { snippet_end })
            .saturating_sub(SOLC_TRUNCATION_CONTEXT)
            .min(text.len());
        text = [&text[..lhs], b" ... ".as_slice(), &text[rhs..]].concat();
        snippet_end = snippet_start + 75;
        length = 75;
    }
    if text.len() > SOLC_MAX_LINE_LENGTH {
        let len = text.len();
        let from = snippet_start.saturating_sub(SOLC_TRUNCATION_CONTEXT).min(len);
        let count = snippet_start.min(SOLC_TRUNCATION_CONTEXT)
            + (length + SOLC_TRUNCATION_CONTEXT).min(len.saturating_sub(snippet_start));
        let mut truncated = text[from..(from + count).min(len)].to_vec();
        if snippet_start + length + SOLC_TRUNCATION_CONTEXT < len {
            truncated.extend_from_slice(b" ...");
        }
        if snippet_start > SOLC_TRUNCATION_CONTEXT {
            truncated = [b" ... ".as_slice(), &truncated].concat();
            snippet_start = 40;
        }
        snippet_end = snippet_start + length;
        text = truncated;
    }
    let snippet_start = snippet_start.min(text.len());
    let snippet_end = snippet_end.min(text.len()).max(snippet_start);

    let line = (line_number + 1).to_string();
    let pad = " ".repeat(line.len());
    let indent = text[..snippet_start]
        .iter()
        .map(|&byte| if byte == b'\t' { '\t' } else { ' ' })
        .collect::<String>();
    let marker = if multiline {
        "^ (Relevant source part starts here and spans across multiple lines).".to_string()
    } else {
        text[snippet_start..snippet_end]
            .iter()
            .map(|&byte| if byte == b'\t' { '\t' } else { '^' })
            .collect()
    };
    format!(
        "{pad}--> {file}:{line}:{column}:\n{pad} |\n{line} | {text}\n{pad} | {indent}{marker}\n",
        file = location.file,
        column = start_column + 1,
        text = String::from_utf8_lossy(&text),
    )
}

/// Returns the zero-based line and byte column of `offset`.
fn line_column(source: &[u8], offset: usize) -> (usize, usize) {
    let offset = offset.min(source.len());
    let before = &source[..offset];
    let line = before.split(|&byte| byte == b'\n').count() - 1;
    let line_start = before.iter().rposition(|&byte| byte == b'\n').map_or(0, |idx| idx + 1);
    (line, offset - line_start)
}

/// Returns the line containing `offset`, without its line terminator.
fn line_at(source: &[u8], offset: usize) -> &[u8] {
    let offset = offset.min(source.len());
    let start = source[..offset].iter().rposition(|&byte| byte == b'\n').map_or(0, |idx| idx + 1);
    let end = source[start..]
        .iter()
        .position(|&byte| byte == b'\n')
        .map_or(source.len(), |idx| start + idx);
    let line = &source[start..end];
    line.strip_suffix(b"\r").unwrap_or(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use foundry_compilers::artifacts::error::{SecondarySourceLocation, SourceLocation};
    use std::{collections::BTreeMap, path::PathBuf};

    const FILE: &str = "src/A.sol";

    /// Diagnostics below were produced from this source by solc 0.8.38 and Solar 0.3.0.
    const SOURCE: &str = r#"// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;
contract A {
    uint256 x;
    function f(uint256 a) public returns (uint256) {
        uint256 b = 1;
        return 2;
    }
    function g() public view returns (uint256) { uint256 x = 1; return x; }
    function h(uint256 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa, uint256 bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb, uint256 ccccccccccccccccccccccccccccccc) public pure {}
    function k() public pure {
        /// @solidity memory-safe-assembly
        assembly { mstore(0, 1) }
    }
}
"#;

    const SHADOW_WARNING: &str = "Warning: This declaration shadows an existing declaration.\n --> src/A.sol:9:50:\n  |\n9 |     function g() public view returns (uint256) { uint256 x = 1; return x; }\n  |                                                  ^^^^^^^^^\nNote: The shadowed declaration is here:\n --> src/A.sol:4:5:\n  |\n4 |     uint256 x;\n  |     ^^^^^^^^^\n\n";

    const SOLAR_MUTABILITY_WARNING: &str = "warning[2018]: function state mutability can be restricted to pure\n  ╭▸ src/A.sol:5:5\n  │\n5 │ ┏     function f(uint256 a) public returns (uint256) {\n6 │ ┃         uint256 b = 1;\n7 │ ┃         return 2;\n8 │ ┃     }\n  ╰╴┗━━━━━┛\n\n";

    /// Probe text inserted after the opening brace of `f` and `g`.
    const PROBE: &str = "/// @solidity memory-safe-assembly\nassembly { mstore(0x00, 1) }\n";

    fn source(text: &str, edits: Vec<SourceEdit>) -> InstrumentedSource {
        InstrumentedSource { original: Arc::new(text.to_string()), edits }
    }

    fn location(source: &InstrumentedSource, range: (usize, usize)) -> OriginalLocation<'_> {
        OriginalLocation { file: FILE, source, start: range.0, end: range.1 }
    }

    /// Inserts [`PROBE`] after the opening brace of `f` and `g` in [`SOURCE`].
    fn instrument() -> (String, Vec<SourceEdit>) {
        let mut instrumented = SOURCE.to_string();
        let mut edits = Vec::<SourceEdit>::new();
        for function in ["function f(uint256 a) public returns (uint256) {", "{ uint256 x"] {
            let start = SOURCE.find(function).unwrap() + function.find('{').unwrap() + 1;
            let shift = edits.iter().map(|edit| edit.len).sum::<usize>();
            instrumented.insert_str(start + shift, PROBE);
            edits.push(SourceEdit { start, end: start, len: PROBE.len() });
        }
        (instrumented, edits)
    }

    /// Translates an original range to the instrumented source.
    fn instrumented_range(edits: &[SourceEdit], (start, end): (usize, usize)) -> (usize, usize) {
        let shift = |offset: usize, inclusive: bool| {
            edits
                .iter()
                .filter(|edit| if inclusive { edit.start <= offset } else { edit.start < offset })
                .map(|edit| edit.len)
                .sum::<usize>()
        };
        (start + shift(start, true), end + shift(end, false))
    }

    fn metadata(edits: Vec<SourceEdit>) -> InstrumentedCoverageMetadata {
        InstrumentedCoverageMetadata {
            probes: Vec::new(),
            sources: BTreeMap::from([(PathBuf::from(FILE), source(SOURCE, edits))]),
        }
    }

    fn warning(range: (usize, usize), message: &str, formatted: String) -> Error {
        Error {
            source_location: Some(SourceLocation {
                file: FILE.to_string(),
                start: range.0 as i32,
                end: range.1 as i32,
            }),
            secondary_source_locations: Vec::new(),
            r#type: "Warning".to_string(),
            component: "general".to_string(),
            severity: Severity::Warning,
            error_code: Some(2018),
            message: message.to_string(),
            formatted_message: Some(formatted),
        }
    }

    #[test]
    fn renders_solc_locations_like_solc() {
        let source = source(SOURCE, Vec::new());
        assert_eq!(
            render_solc_location(&location(&source, (88, 183))),
            " --> src/A.sol:5:5:\n  |\n5 |     function f(uint256 a) public returns (uint256) {\n  |     ^ (Relevant source part starts here and spans across multiple lines).\n"
        );
        assert_eq!(
            render_solc_location(&location(&source, (188, 259))),
            " --> src/A.sol:9:5:\n  |\n9 |     function g() public view returns (uint256) { uint256 x = 1; return x; }\n  |     ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^\n"
        );
        assert_eq!(
            render_solc_location(&location(&source, (532, 557))),
            "  --> src/A.sol:13:9:\n   |\n13 |         assembly { mstore(0, 1) }\n   |         ^^^^^^^^^^^^^^^^^^^^^^^^^\n"
        );
    }

    #[test]
    fn truncates_long_solc_lines_like_solc() {
        const LONG: &str = "// SPDX-License-Identifier: MIT\npragma solidity ^0.8.0;\ncontract A {\n    function h(uint256 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa, uint256 bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb) public pure returns (uint256) { uint256 cccccccccccccccccc = 1; return 1; }\n    function i() public pure returns (uint256) { uint256 dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd = 1; return 1; }\n}\n";
        let source = source(LONG, Vec::new());
        assert_eq!(
            render_solc_location(&location(&source, (84, 131))),
            " --> src/A.sol:4:16:\n  |\n4 |     function h(uint256 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa, uint256 bbbbbbbbbbbbbbbbbbbbbbbbb ...\n  |                ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^\n"
        );
        assert_eq!(
            render_solc_location(&location(&source, (133, 201))),
            " --> src/A.sol:4:65:\n  |\n4 |  ... aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa, uint256 bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb) public pure returns (uint256) { u ...\n  |                                         ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^\n"
        );
        assert_eq!(
            render_solc_location(&location(&source, (235, 261))),
            " --> src/A.sol:4:167:\n  |\n4 |  ... b) public pure returns (uint256) { uint256 cccccccccccccccccc = 1; return 1; }\n  |                                         ^^^^^^^^^^^^^^^^^^^^^^^^^^\n"
        );
        assert_eq!(
            render_solc_location(&location(&source, (328, 486))),
            " --> src/A.sol:5:50:\n  |\n5 |     function i() public pure returns (uint256) { uint256 ddddddddddddddddddddddddddd ... ddddddddddddddddddddddddddddddddddd = 1; return 1; }\n  |                                                  ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^\n"
        );
    }

    #[test]
    fn keeps_solc_tabs_in_markers() {
        let source = source("contract A {\n\tuint256\t x;\n}\n", Vec::new());
        assert_eq!(
            render_solc_location(&location(&source, (14, 24))),
            " --> src/A.sol:2:2:\n  |\n2 | \tuint256\t x;\n  | \t^^^^^^^\t^^\n"
        );
    }

    #[test]
    fn remaps_solc_diagnostic_to_original_source() {
        let (instrumented, edits) = instrument();
        let instrumented_source = source(&instrumented, Vec::new());
        let primary = instrumented_range(&edits, (233, 242));
        let secondary = instrumented_range(&edits, (73, 82));
        assert_ne!(primary, (233, 242));
        let formatted = format!(
            "Warning: This declaration shadows an existing declaration.\n{}Note: The shadowed declaration is here:\n{}\n",
            render_solc_location(&location(&instrumented_source, primary)),
            render_solc_location(&location(&instrumented_source, secondary)),
        );
        let mut error =
            warning(primary, "This declaration shadows an existing declaration.", formatted);
        error.secondary_source_locations.push(SecondarySourceLocation {
            file: Some(FILE.to_string()),
            start: Some(secondary.0 as i32),
            end: Some(secondary.1 as i32),
            message: Some("The shadowed declaration is here:".to_string()),
        });

        assert!(metadata(edits).remap_diagnostic(&mut error));
        assert_eq!(error.formatted_message.as_deref(), Some(SHADOW_WARNING));
        let location = error.source_location.unwrap();
        assert_eq!((location.start, location.end), (233, 242));
        let secondary = &error.secondary_source_locations[0];
        assert_eq!((secondary.start, secondary.end), (Some(73), Some(82)));
    }

    #[test]
    fn remaps_multiline_solc_diagnostic_to_original_source() {
        let (instrumented, edits) = instrument();
        let instrumented_source = source(&instrumented, Vec::new());
        let range = instrumented_range(&edits, (188, 259));
        let message = "Function state mutability can be restricted to pure";
        let formatted = format!(
            "Warning: {message}\n{}\n",
            render_solc_location(&location(&instrumented_source, range))
        );
        assert!(formatted.contains("{/// @solidity memory-safe-assembly"), "{formatted}");
        let mut error = warning(range, message, formatted);

        assert!(metadata(edits).remap_diagnostic(&mut error));
        assert_eq!(
            error.formatted_message.as_deref(),
            Some(
                "Warning: Function state mutability can be restricted to pure\n --> src/A.sol:9:5:\n  |\n9 |     function g() public view returns (uint256) { uint256 x = 1; return x; }\n  |     ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^\n\n"
            )
        );
    }

    #[test]
    fn remaps_solar_diagnostic_to_original_source() {
        let (instrumented, edits) = instrument();
        let instrumented_source = source(&instrumented, Vec::new());
        let range = instrumented_range(&edits, (88, 183));
        let message = "function state mutability can be restricted to pure";
        let mut error = warning(range, message, String::new());
        error.formatted_message = render_solar(&error, &location(&instrumented_source, range), &[]);
        assert!(error.formatted_message.as_ref().unwrap().contains("memory-safe-assembly"));

        assert!(metadata(edits).remap_diagnostic(&mut error));
        assert_eq!(error.formatted_message.as_deref(), Some(SOLAR_MUTABILITY_WARNING));
        let location = error.source_location.unwrap();
        assert_eq!((location.start, location.end), (88, 183));
    }

    #[test]
    fn drops_warnings_about_instrumentation() {
        let (instrumented, edits) = instrument();
        let start = instrumented.find("assembly { mstore(0x00").unwrap();
        let probe = (start, start + "assembly { mstore(0x00, 1) }".len());
        let metadata = metadata(edits);

        let mut error = warning(probe, "Deprecated.", "Warning: Deprecated.\n".to_string());
        assert!(!metadata.remap_diagnostic(&mut error));

        error.severity = Severity::Error;
        assert!(metadata.remap_diagnostic(&mut error));

        let mut error = warning((0, 10), "Library.", "Warning: Library.\n".to_string());
        error.source_location.as_mut().unwrap().file = COVERAGE_LIBRARY_PATH.to_string();
        assert!(!metadata.remap_diagnostic(&mut error));
    }

    #[test]
    fn keeps_diagnostics_on_unedited_sources() {
        let mut error = warning(
            (233, 242),
            "This declaration shadows an existing declaration.",
            SHADOW_WARNING.to_string(),
        );
        assert!(metadata(Vec::new()).remap_diagnostic(&mut error));
        assert_eq!(error.formatted_message.as_deref(), Some(SHADOW_WARNING));
    }
}
