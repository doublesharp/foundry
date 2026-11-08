//! terminal utils
use foundry_compilers::{
    artifacts::remappings::Remapping,
    report::{BasicStdoutReporter, Reporter},
};
use itertools::Itertools;
use semver::Version;
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fmt::Write as _,
    io,
    io::{IsTerminal, prelude::*},
    path::{Path, PathBuf},
    sync::{
        LazyLock, Mutex, PoisonError,
        mpsc::{self, TryRecvError},
    },
    thread,
    time::Duration,
};
use yansi::Paint;

use crate::shell;

/// Some spinners
// https://github.com/gernest/wow/blob/master/spin/spinners.go
pub static SPINNERS: &[&[&str]] = &[
    &["⠃", "⠊", "⠒", "⠢", "⠆", "⠰", "⠔", "⠒", "⠑", "⠘"],
    &[" ", "⠁", "⠉", "⠙", "⠚", "⠖", "⠦", "⠤", "⠠"],
    &["┤", "┘", "┴", "└", "├", "┌", "┬", "┐"],
    &["▹▹▹▹▹", "▸▹▹▹▹", "▹▸▹▹▹", "▹▹▸▹▹", "▹▹▹▸▹", "▹▹▹▹▸"],
    &[" ", "▘", "▀", "▜", "█", "▟", "▄", "▖"],
];

static TERM_SETTINGS: LazyLock<TermSettings> = LazyLock::new(TermSettings::from_env);

/// Helper type to determine the current tty
pub struct TermSettings {
    indicate_progress: bool,
}

impl TermSettings {
    /// Returns a new [`TermSettings`], configured from the current environment.
    ///
    /// Progress is written to stderr (see [`Spinner::tick`]), so it is enabled only
    /// when stderr is a terminal.
    pub fn from_env() -> Self {
        Self { indicate_progress: std::io::stderr().is_terminal() }
    }
}

#[expect(missing_docs)]
pub struct Spinner {
    indicator: &'static [&'static str],
    no_progress: bool,
    message: String,
    idx: usize,
}

#[expect(missing_docs)]
impl Spinner {
    pub fn new(msg: impl Into<String>) -> Self {
        Self::with_indicator(SPINNERS[0], msg)
    }

    pub fn with_indicator(indicator: &'static [&'static str], msg: impl Into<String>) -> Self {
        Self {
            indicator,
            no_progress: !TERM_SETTINGS.indicate_progress,
            message: msg.into(),
            idx: 0,
        }
    }

    pub fn tick(&mut self) {
        if self.no_progress {
            return;
        }

        let indicator = self.indicator[self.idx % self.indicator.len()].green();
        let indicator = Paint::new(format!("[{indicator}]")).bold();
        // Progress is a diagnostic, not data: write to stderr so stdout stays clean
        // for machine-readable output.
        let _ = sh_eprint!("\r\x1B[2K\r{indicator} {}", self.message);
        io::stderr().flush().unwrap();

        self.idx = self.idx.wrapping_add(1);
    }

    pub fn message(&mut self, msg: impl Into<String>) {
        self.message = msg.into();
    }
}

/// A spinner used as [`report::Reporter`]
///
/// This reporter will prefix messages with a spinning cursor
#[derive(Debug)]
#[must_use = "Terminates the spinner on drop"]
pub struct SpinnerReporter {
    /// The sender to the spinner thread.
    sender: mpsc::Sender<SpinnerMsg>,
    /// The project root path for trimming file paths in verbose output.
    project_root: Option<PathBuf>,
    /// Unresolved imports already reported.
    unresolved_imports: UnresolvedImports,
    /// Whether to print the resolved settings for each compiler invocation.
    print_compiler_settings: bool,
}

impl SpinnerReporter {
    /// Spawns the [`Spinner`] on a new thread
    ///
    /// The spinner's message will be updated via the `reporter` events
    ///
    /// On drop the channel will disconnect and the thread will terminate
    pub fn spawn(project_root: Option<PathBuf>) -> Self {
        let (sender, rx) = mpsc::channel::<SpinnerMsg>();

        std::thread::Builder::new()
            .name("spinner".into())
            .spawn(move || {
                let mut spinner = Spinner::new("Compiling...");
                // Only emit the trailing newline (so past messages aren't overwritten by
                // future ticks) when the spinner is actually painting to stderr. When
                // `no_progress` is set the spinner is a no-op, so we shouldn't pollute
                // stderr with blank lines either.
                let emits_progress = !spinner.no_progress;
                loop {
                    spinner.tick();
                    match rx.try_recv() {
                        Ok(SpinnerMsg::Msg(msg)) => {
                            spinner.message(msg);
                            if emits_progress {
                                // new line so past messages are not overwritten
                                // (matches the spinner channel: stderr)
                                let _ = sh_eprintln!();
                            }
                        }
                        Ok(SpinnerMsg::Shutdown(ack)) => {
                            if emits_progress {
                                // end with a newline (matches the spinner channel: stderr)
                                let _ = sh_eprintln!();
                            }
                            let _ = ack.send(());
                            break;
                        }
                        Err(TryRecvError::Disconnected) => break,
                        Err(TryRecvError::Empty) => thread::sleep(Duration::from_millis(100)),
                    }
                }
            })
            .expect("failed to spawn thread");

        Self {
            sender,
            project_root,
            unresolved_imports: Default::default(),
            print_compiler_settings: false,
        }
    }

    /// Sets whether resolved compiler settings are included in progress output.
    pub const fn with_compiler_settings(mut self, yes: bool) -> Self {
        self.print_compiler_settings = yes;
        self
    }

    fn send_msg(&self, msg: impl Into<String>) {
        let _ = self.sender.send(SpinnerMsg::Msg(msg.into()));
    }
}

enum SpinnerMsg {
    Msg(String),
    Shutdown(mpsc::Sender<()>),
}

impl Drop for SpinnerReporter {
    fn drop(&mut self) {
        let (tx, rx) = mpsc::channel();
        if self.sender.send(SpinnerMsg::Shutdown(tx)).is_ok() {
            let _ = rx.recv();
        }
    }
}

impl Reporter for SpinnerReporter {
    fn on_compiler_spawn(&self, compiler_name: &str, version: &Version, dirty_files: &[PathBuf]) {
        // Verbose message with dirty files displays first to avoid being overlapped
        // by the spinner in .tick() which prints repeatedly over the same line.
        if shell::verbosity() >= 5 {
            self.send_msg(format!(
                "Files to compile:\n{}",
                dirty_files
                    .iter()
                    .map(|path| {
                        let trimmed_path = if let Some(project_root) = &self.project_root {
                            path.strip_prefix(project_root).unwrap_or(path)
                        } else {
                            path
                        };
                        format!("- {}", trimmed_path.display())
                    })
                    .sorted()
                    .format("\n")
            ));
        }

        self.send_msg(format!(
            "Compiling {} files with {} {}.{}.{}",
            dirty_files.len(),
            compiler_name,
            version.major,
            version.minor,
            version.patch
        ));
    }

    fn on_compiler_settings(
        &self,
        compiler_name: &str,
        version: &Version,
        profile: &str,
        settings: &str,
    ) {
        if self.print_compiler_settings {
            self.send_msg(format!(
                "Compiler settings for {compiler_name} {}.{}.{} (profile: {profile}): {settings}",
                version.major, version.minor, version.patch
            ));
        }
    }

    fn on_compiler_success(&self, compiler_name: &str, version: &Version, duration: &Duration) {
        self.send_msg(format!(
            "{} {}.{}.{} finished in {duration:.2?}",
            compiler_name, version.major, version.minor, version.patch
        ));
    }

    fn on_solc_installation_start(&self, version: &Version) {
        self.send_msg(format!("Installing Solc version {version}"));
    }

    fn on_solc_installation_success(&self, version: &Version) {
        self.send_msg(format!("Successfully installed Solc {version}"));
    }

    fn on_solc_installation_error(&self, version: &Version, error: &str) {
        self.send_msg(format!("Failed to install Solc {version}: {error}").red().to_string());
    }

    fn on_solar_installation_start(&self, version: &Version) {
        self.send_msg(format!("Installing Solar version {version}"));
    }

    fn on_solar_installation_success(&self, version: &Version) {
        self.send_msg(format!("Successfully installed Solar {version}"));
    }

    fn on_solar_installation_error(&self, version: &Version, error: &str) {
        self.send_msg(format!("Failed to install Solar {version}: {error}").red().to_string());
    }

    fn on_unresolved_imports(&self, imports: &[(&Path, &Path)], remappings: &[Remapping]) {
        if let Some(message) =
            self.unresolved_imports.report(imports, remappings, self.project_root.as_deref())
        {
            self.send_msg(message);
        }
    }
}

/// A non-interactive [`Reporter`] that prints progress like [`BasicStdoutReporter`] and condenses
/// unresolved import reports like [`SpinnerReporter`].
#[derive(Debug)]
pub struct BasicReporter {
    inner: BasicStdoutReporter,
    project_root: Option<PathBuf>,
    unresolved_imports: UnresolvedImports,
}

impl BasicReporter {
    /// Creates a reporter that shortens paths relative to `project_root`.
    pub fn new(project_root: Option<PathBuf>, print_compiler_settings: bool) -> Self {
        Self {
            inner: BasicStdoutReporter::default().with_compiler_settings(print_compiler_settings),
            project_root,
            unresolved_imports: Default::default(),
        }
    }
}

impl Reporter for BasicReporter {
    fn on_compiler_spawn(&self, compiler_name: &str, version: &Version, dirty_files: &[PathBuf]) {
        self.inner.on_compiler_spawn(compiler_name, version, dirty_files);
    }

    fn on_compiler_settings(
        &self,
        compiler_name: &str,
        version: &Version,
        profile: &str,
        settings: &str,
    ) {
        self.inner.on_compiler_settings(compiler_name, version, profile, settings);
    }

    fn on_compiler_success(&self, compiler_name: &str, version: &Version, duration: &Duration) {
        self.inner.on_compiler_success(compiler_name, version, duration);
    }

    fn on_solc_installation_start(&self, version: &Version) {
        self.inner.on_solc_installation_start(version);
    }

    fn on_solc_installation_success(&self, version: &Version) {
        self.inner.on_solc_installation_success(version);
    }

    fn on_solc_installation_error(&self, version: &Version, error: &str) {
        self.inner.on_solc_installation_error(version, error);
    }

    fn on_solar_installation_start(&self, version: &Version) {
        self.inner.on_solar_installation_start(version);
    }

    fn on_solar_installation_success(&self, version: &Version) {
        self.inner.on_solar_installation_success(version);
    }

    fn on_solar_installation_error(&self, version: &Version, error: &str) {
        self.inner.on_solar_installation_error(version, error);
    }

    fn on_unresolved_imports(&self, imports: &[(&Path, &Path)], remappings: &[Remapping]) {
        if let Some(message) =
            self.unresolved_imports.report(imports, remappings, self.project_root.as_deref())
        {
            let _ = sh_println!("{message}");
        }
    }
}

/// Unresolved `(import, importing file)` pairs already reported by a reporter.
///
/// A build can resolve the source graph several times, and each resolution reports the same
/// imports again.
#[derive(Debug, Default)]
struct UnresolvedImports(Mutex<HashSet<(PathBuf, PathBuf)>>);

impl UnresolvedImports {
    /// Formats the unresolved imports that were not reported yet.
    fn report(
        &self,
        imports: &[(&Path, &Path)],
        remappings: &[Remapping],
        project_root: Option<&Path>,
    ) -> Option<String> {
        let mut reported = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let imports = imports
            .iter()
            .copied()
            .filter(|(import, file)| reported.insert((import.to_path_buf(), file.to_path_buf())))
            .collect::<Vec<_>>();
        (!imports.is_empty()).then(|| format_unresolved_imports(&imports, remappings, project_root))
    }
}

/// Formats unresolved imports for an agent or user to fix.
///
/// Each import is listed once with the files that import it. Only remappings whose prefix
/// matches an unresolved import are listed, with a count of the remaining remappings, and
/// remapping targets that do not exist are marked.
pub fn format_unresolved_imports(
    imports: &[(&Path, &Path)],
    remappings: &[Remapping],
    project_root: Option<&Path>,
) -> String {
    let relative = |path: &Path| {
        project_root
            .and_then(|root| path.strip_prefix(root).ok())
            .unwrap_or(path)
            .display()
            .to_string()
    };
    let mut importers = BTreeMap::<String, BTreeSet<String>>::new();
    for (import, file) in imports {
        importers.entry(import.display().to_string()).or_default().insert(relative(file));
    }

    let mut message = String::from("Unable to resolve imports:");
    for (import, files) in &importers {
        let files = files.iter().map(|file| format!("\"{file}\"")).join(", ");
        let _ = write!(message, "\n      \"{import}\" in {files}");
    }

    let matching = remappings
        .iter()
        .filter(|remapping| {
            imports.iter().any(|(import, file)| {
                import.to_string_lossy().starts_with(&remapping.name)
                    && remapping.context.as_deref().is_none_or(|context| {
                        relative(file).starts_with(&relative(Path::new(context)))
                    })
            })
        })
        .collect::<Vec<_>>();
    message.push_str("\nwith remappings:");
    for remapping in &matching {
        let target = Path::new(&remapping.path);
        let target = match project_root {
            Some(root) if target.is_relative() => root.join(target),
            _ => target.to_path_buf(),
        };
        let mut remapping = (*remapping).clone();
        if let Some(root) = project_root {
            remapping.strip_prefix(root);
        }
        let missing = if target.exists() { "" } else { " (target not found)" };
        let _ = write!(message, "\n      {remapping}{missing}");
    }
    let omitted = remappings.len() - matching.len();
    if matching.is_empty() {
        let _ = write!(
            message,
            "\n      no remapping matches these imports ({omitted} configured); \
             run `forge remappings` to list them"
        );
    } else if omitted > 0 {
        let noun = if omitted == 1 { "remapping" } else { "remappings" };
        let _ = write!(
            message,
            "\n      {omitted} other {noun} not shown; run `forge remappings` to list them"
        );
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore]
    fn can_spin() {
        let mut s = Spinner::new("Compiling".to_string());
        let ticks = 50;
        for _ in 0..ticks {
            std::thread::sleep(std::time::Duration::from_millis(100));
            s.tick();
        }
    }

    #[test]
    fn formats_unresolved_imports_with_matching_remappings() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let remappings: Vec<Remapping> = vec![
            format!("@missing/={}/node_modules/@missing/", root.display()).parse().unwrap(),
            "lib/nested/:@oz/=lib/nested/lib/oz/".parse().unwrap(),
            "lib/other/:@oz/=lib/other/lib/oz/".parse().unwrap(),
            "src/=src/".parse().unwrap(),
            "forge-std/=lib/forge-std/src/".parse().unwrap(),
        ];
        let a = root.join("src/A.sol");
        let b = root.join("src/B.sol");
        let nested = root.join("lib/nested/src/N.sol");
        let unresolved = [
            (Path::new("@missing/pkg/Token.sol"), b.as_path()),
            (Path::new("@missing/pkg/Token.sol"), a.as_path()),
            (Path::new("@oz/Ownable.sol"), nested.as_path()),
            (Path::new("./Local.sol"), a.as_path()),
        ];
        snapbox::assert_data_eq!(
            format_unresolved_imports(&unresolved, &remappings, Some(root)),
            snapbox::str![[r#"
Unable to resolve imports:
      "./Local.sol" in "src/A.sol"
      "@missing/pkg/Token.sol" in "src/A.sol", "src/B.sol"
      "@oz/Ownable.sol" in "lib/nested/src/N.sol"
with remappings:
      @missing/=node_modules/@missing/ (target not found)
      lib/nested/:@oz/=lib/nested/lib/oz/ (target not found)
      3 other remappings not shown; run `forge remappings` to list them
"#]]
        );

        snapbox::assert_data_eq!(
            format_unresolved_imports(&unresolved[3..], &remappings, Some(root)),
            snapbox::str![[r#"
Unable to resolve imports:
      "./Local.sol" in "src/A.sol"
with remappings:
      no remapping matches these imports (5 configured); run `forge remappings` to list them
"#]]
        );
    }

    #[test]
    fn reports_each_unresolved_import_once() {
        let file = Path::new("src/ReportedOnce.sol");
        let first = [(Path::new("reported/First.sol"), file)];
        let both = [first[0], (Path::new("reported/Second.sol"), file)];
        let reported = UnresolvedImports::default();
        assert!(reported.report(&first, &[], None).is_some());
        assert!(reported.report(&first, &[], None).is_none());
        snapbox::assert_data_eq!(
            reported.report(&both, &[], None).unwrap(),
            snapbox::str![[r#"
Unable to resolve imports:
      "reported/Second.sol" in "src/ReportedOnce.sol"
with remappings:
      no remapping matches these imports (0 configured); run `forge remappings` to list them
"#]]
        );
    }
}
