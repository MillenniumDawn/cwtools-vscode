//! `cwtools.toml` — the settings a CI job would otherwise repeat on every
//! command line.
//!
//! Discovered by walking up from the target directory (or the process CWD when
//! the command line didn't name one); `--config <path>` skips discovery and
//! uses exactly that file. Command-line flags always win over file values.
//! Relative paths in the file resolve against the file's own directory, so the
//! same config works from any working directory.
//!
//! ```toml
//! game = "hoi4"
//! rules = "../cwtools-hoi4-config/Config"
//! vanilla = "/games/Hearts of Iron IV"
//! vanilla-cache = "ci/vanilla.cwb"
//! no-vanilla-cache = false
//! refresh-vanilla-cache = false
//! directory = "."
//! report-type = "github"
//! min-severity = "warning"
//! fail-on = "error"
//! ignore-files = ["*.notes"]
//! ignore-dirs = ["build", "temp*"]
//! loc-languages = ["english"]
//! ignore-codes = ["CW100", "CW113"]
//! only-codes = []
//! allow-empty = false
//! ```
//!
//! `validate` reads every key. `fix` reads all but `report-type`,
//! `min-severity` and `fail-on` (it writes edits, not a report), and takes
//! `only-codes` as the config spelling of its `--code`. `loc` reads the keys
//! that shape a localisation scan: `game` and `rules` (which turn on the scope
//! checks), `directory`, `report-type`, `min-severity`, `fail-on`,
//! `ignore-files`, `ignore-dirs`, `loc-languages`, the two code lists and
//! `allow-empty`.
//!
//! The parser accepts the TOML this schema needs — comments, bare keys, basic
//! and literal strings, booleans, and (multi-line) string arrays — and rejects
//! everything else by name and line rather than guessing.

use crate::report::ReportType;
use crate::run::FailOn;
use cwtools_localization::Lang;
use cwtools_validation::ErrorSeverity;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// The file name searched for when `--config` isn't given.
pub(crate) const FILE_NAME: &str = "cwtools.toml";

/// Every key the schema defines, in the order the error message lists them.
const KEYS: &[&str] = &[
    "game",
    "directory",
    "rules",
    "vanilla",
    "vanilla-cache",
    "no-vanilla-cache",
    "refresh-vanilla-cache",
    "report-type",
    "min-severity",
    "fail-on",
    "ignore-files",
    "ignore-dirs",
    "loc-languages",
    "case-sensitive-files",
    "ignore-codes",
    "only-codes",
    "allow-empty",
];

/// The keys each subcommand consumes. A file may hold settings for all three, so
/// a key outside the running command's set is reported rather than left to look
/// like it did nothing.
pub(crate) const VALIDATE_KEYS: &[&str] = KEYS;

/// `fix` writes edits, not a report, so the report-shaping keys don't apply.
pub(crate) const FIX_KEYS: &[&str] = &[
    "game",
    "directory",
    "rules",
    "vanilla",
    "vanilla-cache",
    "no-vanilla-cache",
    "refresh-vanilla-cache",
    "ignore-files",
    "ignore-dirs",
    "loc-languages",
    "case-sensitive-files",
    "ignore-codes",
    "only-codes",
    "allow-empty",
];

/// `loc` lints a directory of loc files. It reads the ruleset for its scopes and
/// links, but never indexes the base game, so the vanilla keys don't apply.
pub(crate) const LOC_KEYS: &[&str] = &[
    "game",
    "directory",
    "rules",
    "report-type",
    "min-severity",
    "fail-on",
    "ignore-files",
    "ignore-dirs",
    "loc-languages",
    "ignore-codes",
    "only-codes",
    "allow-empty",
];

/// A config file that can't be used. Always names the file (and the line, for a
/// syntax problem) so a broken config in CI is diagnosable from the log alone.
#[derive(Debug, thiserror::Error)]
#[error("{location}: {message}")]
pub(crate) struct ConfigError {
    location: String,
    message: String,
}

impl ConfigError {
    fn at(path: &Path, line: usize, message: impl Into<String>) -> Self {
        Self {
            location: format!("{}:{}", path.display(), line),
            message: message.into(),
        }
    }

    fn file(path: &Path, message: impl Into<String>) -> Self {
        Self {
            location: path.display().to_string(),
            message: message.into(),
        }
    }
}

/// A parsed `cwtools.toml`. Values are validated at load, so a bad game id or
/// severity fails naming the file rather than surfacing as a confusing run.
#[derive(Debug, Default)]
pub(crate) struct FileConfig {
    /// Absolute path of the file the values came from.
    pub(crate) path: PathBuf,
    /// Every key the file actually set, so a command can name the ones it won't
    /// act on instead of ignoring them quietly.
    pub(crate) present: Vec<&'static str>,
    pub(crate) game: Option<String>,
    pub(crate) directory: Option<PathBuf>,
    pub(crate) rules: Option<PathBuf>,
    pub(crate) vanilla: Option<PathBuf>,
    pub(crate) vanilla_cache: Option<PathBuf>,
    pub(crate) no_vanilla_cache: bool,
    pub(crate) refresh_vanilla_cache: bool,
    pub(crate) report_type: Option<ReportType>,
    pub(crate) min_severity: Option<ErrorSeverity>,
    pub(crate) fail_on: Option<FailOn>,
    pub(crate) ignore_files: Vec<String>,
    pub(crate) ignore_dirs: Vec<String>,
    pub(crate) loc_languages: Vec<Lang>,
    pub(crate) case_sensitive_files: Option<bool>,
    pub(crate) ignore_codes: Vec<String>,
    pub(crate) only_codes: Vec<String>,
    pub(crate) allow_empty: bool,
}

/// Resolve the config governing a run. `explicit` is `--config` and must exist;
/// otherwise walk up from `anchor` (the target directory, or the CWD when the
/// command line didn't name one). `Ok(None)` means no config file was found,
/// i.e. the run is flags-only.
pub(crate) fn resolve(
    explicit: Option<&Path>,
    anchor: Option<&Path>,
) -> Result<Option<FileConfig>, ConfigError> {
    let path = match explicit {
        Some(p) => {
            if !p.is_file() {
                return Err(ConfigError::file(
                    &absolute(p),
                    "no such config file (--config must name a readable file)",
                ));
            }
            p.to_path_buf()
        }
        None => {
            let start = anchor.map_or_else(
                || std::env::current_dir().unwrap_or_default(),
                Path::to_path_buf,
            );
            match discover(&absolute(&start)) {
                Some(p) => p,
                None => return Ok(None),
            }
        }
    };
    load(&path).map(Some)
}

/// The first `cwtools.toml` at or above `dir`.
fn discover(dir: &Path) -> Option<PathBuf> {
    dir.ancestors()
        .map(|d| d.join(FILE_NAME))
        .find(|p| p.is_file())
}

/// Read and validate one config file.
pub(crate) fn load(path: &Path) -> Result<FileConfig, ConfigError> {
    let path = absolute(path);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| ConfigError::file(&path, format!("could not read config: {e}")))?;
    let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let entries = parse(&text, &path)?;
    from_entries(path.clone(), &dir, entries)
}

/// `path` made absolute where that can be computed, so every message names a
/// location the reader can act on.
fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

// ── Merge helpers ────────────────────────────────────────────────────────────

/// Record `key` as taken from the file. The key is what the "Using config …"
/// line prints, so a call site that misspells one would report a setting the
/// schema doesn't have.
fn note(key: &'static str, applied: &mut Vec<&'static str>) {
    debug_assert!(KEYS.contains(&key), "`{key}` is not a cwtools.toml key");
    applied.push(key);
}

/// Resolve one optional setting: the flag wins, and a value taken from the file
/// is recorded under `key` for the "Using config …" line.
pub(crate) fn pick<T>(
    flag: Option<T>,
    file: Option<T>,
    key: &'static str,
    applied: &mut Vec<&'static str>,
) -> Option<T> {
    match (flag, file) {
        (Some(v), _) => Some(v),
        (None, Some(v)) => {
            note(key, applied);
            Some(v)
        }
        (None, None) => None,
    }
}

/// Repeatable list settings: any occurrence of the flag replaces the file's
/// list outright, so a command line never half-merges with a config.
pub(crate) fn pick_list<T>(
    flag: Vec<T>,
    file: Vec<T>,
    key: &'static str,
    applied: &mut Vec<&'static str>,
) -> Vec<T> {
    if !flag.is_empty() || file.is_empty() {
        return flag;
    }
    note(key, applied);
    file
}

/// Switch settings. A flag can only turn one on, so a file `true` and the flag
/// OR together; there is no command-line way back to `false`.
pub(crate) fn pick_flag(
    flag: bool,
    file: bool,
    key: &'static str,
    applied: &mut Vec<&'static str>,
) -> bool {
    if file && !flag {
        note(key, applied);
    }
    flag || file
}

/// Resolve a default-true boolean (CW113 case-sensitivity): an explicit CLI
/// value wins, then an explicit config-file value, then `true`. `flag` is `None`
/// when the flag wasn't passed; `file` is `None` when the key wasn't set.
pub(crate) fn pick_flag_default(
    flag: Option<bool>,
    file: Option<bool>,
    key: &'static str,
    applied: &mut Vec<&'static str>,
) -> bool {
    if let Some(v) = flag {
        return v;
    }
    if let Some(v) = file {
        note(key, applied);
        return v;
    }
    true
}

// ── Schema ───────────────────────────────────────────────────────────────────

fn from_entries(path: PathBuf, dir: &Path, entries: Vec<Entry>) -> Result<FileConfig, ConfigError> {
    let mut cfg = FileConfig {
        path,
        ..FileConfig::default()
    };
    let mut seen: HashSet<&str> = HashSet::new();
    for e in &entries {
        let key = KEYS
            .iter()
            .find(|k| **k == e.key)
            .ok_or_else(|| unknown_key(&cfg.path, e))?;
        if !seen.insert(key) {
            return Err(ConfigError::at(
                &cfg.path,
                e.line,
                format!("duplicate key `{}`", e.key),
            ));
        }
        cfg.present.push(key);
        match *key {
            "game" => {
                let v = string(&cfg.path, e)?;
                if cwtools_game::constants::Game::from_str(&v).is_none() {
                    return Err(bad_value(&cfg.path, e, "unknown game", &v));
                }
                cfg.game = Some(v);
            }
            "directory" => cfg.directory = Some(path_value(&cfg.path, dir, e)?),
            "rules" => cfg.rules = Some(path_value(&cfg.path, dir, e)?),
            "vanilla" => cfg.vanilla = Some(path_value(&cfg.path, dir, e)?),
            "vanilla-cache" => cfg.vanilla_cache = Some(path_value(&cfg.path, dir, e)?),
            "no-vanilla-cache" => cfg.no_vanilla_cache = boolean(&cfg.path, e)?,
            "refresh-vanilla-cache" => cfg.refresh_vanilla_cache = boolean(&cfg.path, e)?,
            "allow-empty" => cfg.allow_empty = boolean(&cfg.path, e)?,
            "case-sensitive-files" => cfg.case_sensitive_files = Some(boolean(&cfg.path, e)?),
            "report-type" => {
                let v = string(&cfg.path, e)?;
                cfg.report_type = Some(
                    crate::report::parse_report_type(&v)
                        .map_err(|m| ConfigError::at(&cfg.path, e.line, m))?,
                );
            }
            "min-severity" => {
                let v = string(&cfg.path, e)?;
                cfg.min_severity = Some(
                    crate::cli::parse_min_severity(&v)
                        .map_err(|m| ConfigError::at(&cfg.path, e.line, m))?,
                );
            }
            "fail-on" => {
                let v = string(&cfg.path, e)?;
                cfg.fail_on = Some(
                    crate::run::parse_fail_on(&v)
                        .map_err(|m| ConfigError::at(&cfg.path, e.line, m))?,
                );
            }
            "ignore-files" => cfg.ignore_files = strings(&cfg.path, e)?,
            "ignore-dirs" => cfg.ignore_dirs = strings(&cfg.path, e)?,
            "loc-languages" => {
                cfg.loc_languages = list(&cfg.path, e)?
                    .iter()
                    .map(|v| {
                        crate::cli::parse_lang(&v.value)
                            .map_err(|m| ConfigError::at(&cfg.path, v.line, m))
                    })
                    .collect::<Result<_, _>>()?;
            }
            "ignore-codes" => cfg.ignore_codes = codes(&cfg.path, e)?,
            "only-codes" => cfg.only_codes = codes(&cfg.path, e)?,
            _ => unreachable!("key came from KEYS"),
        }
    }
    Ok(cfg)
}

/// A path setting, resolved against the config file's own directory. `join`
/// leaves an absolute value alone, which is what a shared CI config wants.
///
/// The result is lexically cleaned so `directory = "."` yields the config's
/// directory rather than `<dir>/.`: diagnostic hashes key on the file string, so
/// a stray `/./` in every path would invalidate an existing `--ignore-hashes`
/// baseline. `..` is deliberately left for the OS to resolve — folding it here
/// would take the wrong branch through a symlinked directory.
fn path_value(path: &Path, dir: &Path, e: &Entry) -> Result<PathBuf, ConfigError> {
    let raw = string(path, e)?;
    if raw.is_empty() {
        // `dir.join("")` would silently mean "the config's own directory".
        return Err(ConfigError::at(
            path,
            e.line,
            format!("key `{}` is empty; give it a path", e.key),
        ));
    }
    let joined: PathBuf = dir.join(raw).components().collect();
    Ok(joined)
}

fn codes(path: &Path, e: &Entry) -> Result<Vec<String>, ConfigError> {
    list(path, e)?
        .iter()
        .map(|v| crate::codes::parse_code(&v.value).map_err(|m| ConfigError::at(path, v.line, m)))
        .collect()
}

fn unknown_key(path: &Path, e: &Entry) -> ConfigError {
    ConfigError::at(
        path,
        e.line,
        format!(
            "unknown key `{}`; expected one of {}",
            e.key,
            KEYS.join(", ")
        ),
    )
}

fn bad_value(path: &Path, e: &Entry, what: &str, value: &str) -> ConfigError {
    ConfigError::at(
        path,
        e.line,
        format!("{what} `{value}` for key `{}`", e.key),
    )
}

fn wrong_type(path: &Path, e: &Entry, want: &str) -> ConfigError {
    ConfigError::at(
        path,
        e.line,
        format!("key `{}` expects {want}, found {}", e.key, e.value.kind()),
    )
}

fn string(path: &Path, e: &Entry) -> Result<String, ConfigError> {
    match &e.value {
        Value::Str(s) => Ok(s.clone()),
        _ => Err(wrong_type(path, e, "a string")),
    }
}

fn boolean(path: &Path, e: &Entry) -> Result<bool, ConfigError> {
    match &e.value {
        Value::Bool(b) => Ok(*b),
        _ => Err(wrong_type(path, e, "a boolean")),
    }
}

fn list<'a>(path: &Path, e: &'a Entry) -> Result<&'a [Listed], ConfigError> {
    match &e.value {
        Value::List(v) => Ok(v),
        _ => Err(wrong_type(path, e, "an array of strings")),
    }
}

/// The plain strings of a list setting, for the keys with no per-entry validation.
fn strings(path: &Path, e: &Entry) -> Result<Vec<String>, ConfigError> {
    Ok(list(path, e)?.iter().map(|l| l.value.clone()).collect())
}

// ── TOML subset parser ───────────────────────────────────────────────────────

#[derive(Debug)]
enum Value {
    Str(String),
    Bool(bool),
    List(Vec<Listed>),
}

/// One array entry with the line it sits on, so a bad code or language in a
/// multi-line list is reported where the reader will find it.
#[derive(Debug)]
struct Listed {
    value: String,
    line: usize,
}

impl Value {
    fn kind(&self) -> &'static str {
        match self {
            Value::Str(_) => "a string",
            Value::Bool(_) => "a boolean",
            Value::List(_) => "an array",
        }
    }
}

#[derive(Debug)]
struct Entry {
    key: String,
    line: usize,
    value: Value,
}

struct Reader<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
    path: &'a Path,
    line: usize,
}

impl<'a> Reader<'a> {
    fn peek(&mut self) -> Option<char> {
        self.chars.peek().copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.chars.next();
        if c == Some('\n') {
            self.line += 1;
        }
        c
    }

    fn err(&self, message: impl Into<String>) -> ConfigError {
        ConfigError::at(self.path, self.line, message)
    }

    /// Spaces and tabs only — never crosses a line break.
    fn skip_blanks(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\r')) {
            self.bump();
        }
    }

    /// Whitespace (including line breaks) and comments.
    fn skip_trivia(&mut self) {
        loop {
            match self.peek() {
                Some(' ' | '\t' | '\r' | '\n') => {
                    self.bump();
                }
                Some('#') => {
                    while !matches!(self.peek(), None | Some('\n')) {
                        self.bump();
                    }
                }
                _ => return,
            }
        }
    }

    /// The rest of the line must be blank or a comment; anything else means the
    /// value ended earlier than the writer thought it did.
    fn end_of_line(&mut self) -> Result<(), ConfigError> {
        self.skip_blanks();
        match self.peek() {
            None | Some('\n') | Some('#') => Ok(()),
            Some(c) => Err(self.err(format!("unexpected `{c}` after the value"))),
        }
    }

    fn key(&mut self) -> Result<String, ConfigError> {
        let mut key = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                key.push(c);
                self.bump();
            } else {
                break;
            }
        }
        if key.is_empty() {
            let c = self.peek().unwrap_or(' ');
            return Err(self.err(format!("expected a key, found `{c}`")));
        }
        match self.peek() {
            Some('.') => Err(self.err(format!(
                "dotted key `{key}.…` is not supported; every cwtools.toml key is top-level"
            ))),
            // Anything else that can't end a key would otherwise surface as a
            // confusing "expected `=` after key `<prefix>`".
            Some(c) if !matches!(c, '=' | ' ' | '\t' | '\r' | '\n' | '#') => Err(self.err(
                format!("invalid character `{c}` in key `{key}{c}…`; keys are [A-Za-z0-9_-]"),
            )),
            _ => Ok(key),
        }
    }

    fn value(&mut self) -> Result<Value, ConfigError> {
        match self.peek() {
            Some('"') | Some('\'') => self.string().map(Value::Str),
            Some('[') => self.array().map(Value::List),
            Some('{') => Err(self.err("inline tables are not supported")),
            _ => {
                let word = self.word();
                match word.as_str() {
                    "true" => Ok(Value::Bool(true)),
                    "false" => Ok(Value::Bool(false)),
                    "" => Err(self.err("expected a value")),
                    other => Err(self.err(format!(
                        "expected a quoted string, `true`/`false`, or an array of strings, \
                         found `{other}`"
                    ))),
                }
            }
        }
    }

    /// A bare token, for recognising `true`/`false` and quoting whatever else
    /// the writer put there back at them.
    fn word(&mut self) -> String {
        let mut out = String::new();
        while let Some(c) = self.peek() {
            if c == '\n' || c == '#' || c == ',' || c == ']' {
                break;
            }
            out.push(c);
            self.bump();
        }
        out.trim_end().to_string()
    }

    fn string(&mut self) -> Result<String, ConfigError> {
        let quote = self.bump().expect("caller peeked the quote");
        // The line the string opened on: the scan may cross the newline that
        // proves it was never closed, and pointing at the next line is no help.
        let opened = self.line;
        let unterminated = || ConfigError::at(self.path, opened, "unterminated string");
        let mut out = String::new();
        loop {
            match self.bump() {
                None | Some('\n') => return Err(unterminated()),
                Some(c) if c == quote => return Ok(out),
                // Literal strings take every byte as-is, which is what a Windows
                // path wants: 'C:\Games\HOI4'.
                Some('\\') if quote == '"' => match self.bump() {
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some('n') => out.push('\n'),
                    Some('r') => out.push('\r'),
                    Some('t') => out.push('\t'),
                    Some(c) => {
                        return Err(self.err(format!(
                            "unknown escape `\\{c}`; use a literal string ('…') for a \
                             Windows path"
                        )));
                    }
                    None => return Err(unterminated()),
                },
                Some(c) => out.push(c),
            }
        }
    }

    fn array(&mut self) -> Result<Vec<Listed>, ConfigError> {
        self.bump(); // '['
        let opened = self.line;
        let unterminated = || ConfigError::at(self.path, opened, "unterminated array");
        let mut out = Vec::new();
        loop {
            self.skip_trivia();
            match self.peek() {
                None => return Err(unterminated()),
                Some(']') => {
                    self.bump();
                    return Ok(out);
                }
                Some('"') | Some('\'') => {
                    let line = self.line;
                    out.push(Listed {
                        value: self.string()?,
                        line,
                    });
                }
                Some(c) => {
                    return Err(
                        self.err(format!("array entries must be quoted strings, found `{c}`"))
                    );
                }
            }
            self.skip_trivia();
            match self.peek() {
                Some(',') => {
                    self.bump();
                }
                Some(']') => {
                    self.bump();
                    return Ok(out);
                }
                None => return Err(unterminated()),
                Some(c) => return Err(self.err(format!("expected `,` or `]`, found `{c}`"))),
            }
        }
    }
}

fn parse(text: &str, path: &Path) -> Result<Vec<Entry>, ConfigError> {
    // Paradox tooling is Windows-first and its loc files must carry a BOM, so a
    // hand-edited cwtools.toml routinely has one. TOML permits it.
    let mut r = Reader {
        chars: text.trim_start_matches('\u{feff}').chars().peekable(),
        path,
        line: 1,
    };
    let mut entries = Vec::new();
    loop {
        r.skip_trivia();
        match r.peek() {
            None => return Ok(entries),
            Some('[') => {
                return Err(
                    r.err("table headers are not supported; every cwtools.toml key is top-level")
                );
            }
            _ => {}
        }
        let line = r.line;
        let key = r.key()?;
        r.skip_blanks();
        if r.peek() != Some('=') {
            return Err(ConfigError::at(
                path,
                line,
                format!("expected `=` after key `{key}`"),
            ));
        }
        r.bump();
        r.skip_blanks();
        let value = r.value()?;
        r.end_of_line()?;
        entries.push(Entry { key, line, value });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, body: &str) -> PathBuf {
        let p = dir.join(FILE_NAME);
        std::fs::write(&p, body).unwrap();
        p
    }

    /// An absolute path spelled the host's way: on Windows a leading `/` names
    /// the current drive's root, so it is still resolved against the config.
    fn abs(tail: &str) -> String {
        if cfg!(windows) {
            format!("C:/{tail}")
        } else {
            format!("/{tail}")
        }
    }

    #[test]
    fn loads_every_key() {
        let tmp = tempfile::tempdir().unwrap();
        let vanilla = abs("games/hoi4");
        let p = write(
            tmp.path(),
            &format!(
                r#"
# a cwtools.toml
game = "hoi4"
directory = "mod"
rules = "../config/Config"
vanilla = "{vanilla}"
vanilla-cache = "ci/vanilla.cwb"
no-vanilla-cache = true
refresh-vanilla-cache = false
report-type = "github"
min-severity = "warning"
fail-on = "none"
ignore-files = ["*.notes"]
ignore-dirs = [
    "build",   # trailing comma and comments are fine
    "temp*",
]
loc-languages = ["english"]
ignore-codes = ["cw100"]
only-codes = []
allow-empty = true
"#
            ),
        );
        let cfg = load(&p).unwrap();
        assert_eq!(cfg.game.as_deref(), Some("hoi4"));
        assert_eq!(cfg.directory, Some(tmp.path().join("mod")));
        assert_eq!(cfg.vanilla, Some(PathBuf::from(&vanilla)));
        assert!(cfg.no_vanilla_cache);
        assert!(!cfg.refresh_vanilla_cache);
        assert!(cfg.allow_empty);
        assert_eq!(cfg.report_type, Some(ReportType::Github));
        assert_eq!(cfg.min_severity, Some(ErrorSeverity::Warning));
        assert_eq!(cfg.fail_on, Some(FailOn::Never));
        assert_eq!(cfg.ignore_files, ["*.notes"]);
        assert_eq!(cfg.ignore_dirs, ["build", "temp*"]);
        assert_eq!(cfg.loc_languages, [Lang::English]);
        // Codes are normalised to the catalog spelling.
        assert_eq!(cfg.ignore_codes, ["CW100"]);
        assert!(cfg.only_codes.is_empty());
    }

    #[test]
    fn relative_paths_resolve_against_the_config_not_the_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), "rules = \"Config\"\ndirectory = \".\"\n");
        let cfg = load(&p).unwrap();
        assert_eq!(cfg.rules, Some(tmp.path().join("Config")));
        assert_eq!(cfg.directory, Some(tmp.path().join(".")));
    }

    /// `directory = "."` must be the config's directory, not `<dir>/.`:
    /// diagnostic hashes key on the file string, so a stray `/./` in every path
    /// would silently invalidate an existing --ignore-hashes baseline.
    #[test]
    fn a_dot_directory_does_not_leak_into_the_path() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), "directory = \".\"\nrules = \"./Config\"\n");
        let cfg = load(&p).unwrap();
        assert_eq!(cfg.directory.as_deref(), Some(tmp.path()));
        assert_eq!(cfg.rules, Some(tmp.path().join("Config")));
    }

    /// `..` is left for the OS: folding it textually would take the wrong branch
    /// through a symlinked directory.
    #[test]
    fn parent_components_are_left_for_the_os() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), "rules = \"../Config\"\n");
        assert_eq!(load(&p).unwrap().rules, Some(tmp.path().join("../Config")));
    }

    #[test]
    fn absolute_paths_are_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let vanilla = abs("opt/hoi4");
        let p = write(tmp.path(), &format!("vanilla = \"{vanilla}\"\n"));
        assert_eq!(load(&p).unwrap().vanilla, Some(PathBuf::from(&vanilla)));
    }

    /// Literal strings take every byte as-is; basic strings unescape. A Windows
    /// path is unusable in a basic string without doubling every separator.
    #[test]
    fn literal_strings_keep_backslashes() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(
            tmp.path(),
            "ignore-files = ['C:\\Games\\*', \"C:\\\\Games\\\\*\"]\n",
        );
        assert_eq!(load(&p).unwrap().ignore_files, ["C:\\Games\\*"; 2]);
    }

    fn err_for(body: &str) -> String {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), body);
        let e = load(&p).unwrap_err().to_string();
        assert!(e.contains(FILE_NAME), "error must name the file: {e}");
        e
    }

    #[test]
    fn unknown_key_is_an_error_naming_the_line() {
        let e = err_for("game = \"hoi4\"\ngmae = \"hoi4\"\n");
        assert!(e.contains(":2:"), "got: {e}");
        assert!(e.contains("unknown key `gmae`"), "got: {e}");
        assert!(e.contains("ignore-codes"), "lists the valid keys: {e}");
    }

    #[test]
    fn wrong_type_is_an_error() {
        let e = err_for("game = true\n");
        assert!(e.contains("expects a string, found a boolean"), "got: {e}");
    }

    #[test]
    fn unquoted_value_is_an_error() {
        let e = err_for("game = hoi4\n");
        assert!(e.contains("expected a quoted string"), "got: {e}");
    }

    #[test]
    fn unknown_game_is_an_error() {
        let e = err_for("game = \"hoi5\"\n");
        assert!(e.contains("unknown game `hoi5`"), "got: {e}");
    }

    #[test]
    fn unknown_code_is_an_error() {
        let e = err_for("ignore-codes = [\"CW999\"]\n");
        assert!(e.contains("unknown diagnostic code 'CW999'"), "got: {e}");
    }

    /// A multi-line array must report the offending entry's line, not the key's.
    #[test]
    fn array_entry_errors_name_the_entry_line() {
        let e = err_for("game = \"hoi4\"\nignore-codes = [\n  \"CW100\",\n  \"CW999\",\n]\n");
        assert!(e.contains(":4:"), "got: {e}");
        let e = err_for("game = \"hoi4\"\nloc-languages = [\n  \"english\",\n  \"klingon\",\n]\n");
        assert!(e.contains(":4:"), "got: {e}");
    }

    #[test]
    fn an_invalid_key_character_is_named() {
        let e = err_for("gäme = \"hoi4\"\n");
        assert!(e.contains("invalid character `ä` in key"), "got: {e}");
    }

    #[test]
    fn unknown_severity_is_an_error() {
        let e = err_for("min-severity = \"critical\"\n");
        assert!(e.contains("invalid severity 'critical'"), "got: {e}");
    }

    #[test]
    fn unknown_fail_on_is_an_error() {
        let e = err_for("fail-on = \"critical\"\n");
        assert!(e.contains("invalid severity 'critical'"), "got: {e}");
        assert!(e.contains("none"), "got: {e}");
    }

    #[test]
    fn unknown_report_type_is_an_error() {
        let e = err_for("report-type = \"sarrif\"\n");
        assert!(e.contains("invalid report type 'sarrif'"), "got: {e}");
    }

    #[test]
    fn duplicate_key_is_an_error() {
        let e = err_for("game = \"hoi4\"\ngame = \"eu4\"\n");
        assert!(e.contains("duplicate key `game`"), "got: {e}");
    }

    #[test]
    fn table_header_is_an_error() {
        let e = err_for("[validate]\ngame = \"hoi4\"\n");
        assert!(e.contains("table headers are not supported"), "got: {e}");
    }

    #[test]
    fn missing_equals_is_an_error() {
        let e = err_for("game \"hoi4\"\n");
        assert!(e.contains("expected `=` after key `game`"), "got: {e}");
    }

    #[test]
    fn unterminated_string_is_an_error_on_its_own_line() {
        let e = err_for("# lead\ngame = \"hoi4\n");
        assert!(e.contains("unterminated string"), "got: {e}");
        assert!(e.contains(":2:"), "points at the opening line: {e}");
    }

    #[test]
    fn unterminated_array_is_an_error_on_its_own_line() {
        let e = err_for("# lead\nignore-dirs = [\"build\",\n");
        assert!(e.contains("unterminated array"), "got: {e}");
        assert!(e.contains(":2:"), "points at the opening line: {e}");
    }

    /// A Windows-edited config routinely carries one, and TOML permits it.
    #[test]
    fn a_leading_bom_is_accepted() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), "\u{feff}game = \"hoi4\"\n");
        assert_eq!(load(&p).unwrap().game.as_deref(), Some("hoi4"));
    }

    /// CRLF is what a Windows editor writes.
    #[test]
    fn crlf_line_endings_parse() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), "game = \"hoi4\"\r\nallow-empty = true\r\n");
        let cfg = load(&p).unwrap();
        assert_eq!(cfg.game.as_deref(), Some("hoi4"));
        assert!(cfg.allow_empty);
    }

    #[test]
    fn trailing_text_after_a_value_is_an_error() {
        let e = err_for("game = \"hoi4\" oops\n");
        assert!(e.contains("unexpected `o` after the value"), "got: {e}");
    }

    #[test]
    fn an_empty_path_is_an_error() {
        let e = err_for("rules = \"\"\n");
        assert!(e.contains("key `rules` is empty"), "got: {e}");
    }

    #[test]
    fn an_empty_config_is_valid_and_sets_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), "# nothing here\n");
        let cfg = load(&p).unwrap();
        assert!(cfg.game.is_none() && cfg.ignore_codes.is_empty() && !cfg.allow_empty);
    }

    #[test]
    fn explicit_config_must_exist() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("nope.toml");
        let e = resolve(Some(&missing), None).unwrap_err().to_string();
        assert!(e.contains("no such config file"), "got: {e}");
    }

    #[test]
    fn discovery_walks_up_from_the_anchor() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "game = \"hoi4\"\n");
        let deep = tmp.path().join("mod").join("common");
        std::fs::create_dir_all(&deep).unwrap();
        let cfg = resolve(None, Some(&deep)).unwrap().unwrap();
        assert_eq!(cfg.game.as_deref(), Some("hoi4"));
    }

    #[test]
    fn discovery_finds_nothing_under_an_empty_tree() {
        // A tempdir under the OS temp root, which has no cwtools.toml above it.
        let tmp = tempfile::tempdir().unwrap();
        assert!(resolve(None, Some(tmp.path())).unwrap().is_none());
    }

    #[test]
    fn explicit_config_beats_discovery() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "game = \"hoi4\"\n");
        let other = tmp.path().join("ci.toml");
        std::fs::write(&other, "game = \"eu4\"\n").unwrap();
        let cfg = resolve(Some(&other), Some(tmp.path())).unwrap().unwrap();
        assert_eq!(cfg.game.as_deref(), Some("eu4"));
    }

    #[test]
    fn flags_win_over_file_values() {
        let mut applied = Vec::new();
        assert_eq!(
            pick(Some("flag"), Some("file"), "game", &mut applied),
            Some("flag")
        );
        assert!(applied.is_empty(), "an overridden key is not 'applied'");
        assert_eq!(pick(None, Some("file"), "game", &mut applied), Some("file"));
        assert_eq!(applied, ["game"]);
    }

    #[test]
    fn a_repeated_flag_replaces_the_file_list() {
        let mut applied = Vec::new();
        let flag = vec!["a".to_string()];
        let file = vec!["b".to_string(), "c".to_string()];
        assert_eq!(
            pick_list(flag, file.clone(), "ignore-dirs", &mut applied),
            ["a"]
        );
        assert!(applied.is_empty());
        assert_eq!(
            pick_list(Vec::new(), file, "ignore-dirs", &mut applied),
            ["b", "c"]
        );
        assert_eq!(applied, ["ignore-dirs"]);
    }

    /// A per-command key set that drifted from the schema would make the
    /// "does not read" warning fire on a key the command actually honours.
    #[test]
    fn every_command_key_set_is_part_of_the_schema() {
        for (name, set) in [
            ("validate", VALIDATE_KEYS),
            ("fix", FIX_KEYS),
            ("loc", LOC_KEYS),
        ] {
            for key in set {
                assert!(KEYS.contains(key), "{name} reads unknown key `{key}`");
            }
        }
    }

    #[test]
    fn present_records_every_key_the_file_set() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), "game = \"hoi4\"\nallow-empty = true\n");
        assert_eq!(load(&p).unwrap().present, ["game", "allow-empty"]);
    }

    #[test]
    fn switches_or_together() {
        let mut applied = Vec::new();
        assert!(pick_flag(true, false, "allow-empty", &mut applied));
        assert!(applied.is_empty());
        assert!(pick_flag(false, true, "allow-empty", &mut applied));
        assert_eq!(applied, ["allow-empty"]);
        assert!(!pick_flag(false, false, "allow-empty", &mut applied));
    }

    #[test]
    fn case_sensitive_defaults_true() {
        // Explicit CLI value wins over the config value.
        let mut applied = Vec::new();
        assert!(!pick_flag_default(
            Some(false),
            Some(true),
            "case-sensitive-files",
            &mut applied
        ));
        assert!(pick_flag_default(
            Some(true),
            Some(false),
            "case-sensitive-files",
            &mut applied
        ));
        // A config value is used (and noted) only when the CLI flag is absent.
        let mut applied = Vec::new();
        assert!(pick_flag_default(
            None,
            Some(true),
            "case-sensitive-files",
            &mut applied
        ));
        assert_eq!(applied, ["case-sensitive-files"]);
        assert!(!pick_flag_default(
            None,
            Some(false),
            "case-sensitive-files",
            &mut applied
        ));
        // Neither set: the default is true.
        assert!(pick_flag_default(
            None,
            None,
            "case-sensitive-files",
            &mut applied
        ));
    }
}
