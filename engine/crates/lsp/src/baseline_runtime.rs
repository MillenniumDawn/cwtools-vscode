use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use tower_lsp::jsonrpc::{Error, ErrorCode, Result};
use tower_lsp::lsp_types::{Diagnostic, MessageType, ProgressToken, Url};

use crate::baseline::{self, BaselineStatus};

#[cfg(test)]
#[path = "baseline_tests.rs"]
mod tests;

#[derive(Default)]
pub(crate) struct State {
    pub(crate) setting: String,
    pub(crate) file: Option<PathBuf>,
    status: Arc<BaselineStatus>,
    error: Option<String>,
    epoch: u64,
    counts: HashMap<String, usize>,
    total: usize,
    capture: Option<Capture>,
    last_written: Option<PathBuf>,
}

#[derive(Default)]
struct Capture {
    files: HashMap<String, (Vec<String>, SourceSnapshot)>,
    target: Option<PathBuf>,
    temporary: Option<PathBuf>,
    failed: bool,
}

#[derive(Clone)]
struct SourceSnapshot {
    path: PathBuf,
    hash: u64,
    version: Option<i32>,
}

pub(crate) struct Prepared {
    epoch: u64,
    hashes: Option<Vec<(String, String)>>,
    source: Option<SourceSnapshot>,
    source_required: bool,
}

struct PendingBaseline {
    temporary: PathBuf,
    target: PathBuf,
}

impl Drop for PendingBaseline {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.temporary);
    }
}

struct CaptureGuard(Arc<crate::DocumentState>);

impl Drop for CaptureGuard {
    fn drop(&mut self) {
        self.0.baseline.lock().capture = None;
    }
}

pub(crate) enum StatusNotification {}

impl tower_lsp::lsp_types::notification::Notification for StatusNotification {
    type Params = serde_json::Value;
    const METHOD: &'static str = "diagnosticsBaselineStatus";
}

fn failure(message: impl ToString) -> Error {
    Error {
        code: ErrorCode::ServerError(-32803),
        message: message.to_string().into(),
        data: None,
    }
}

impl State {
    pub(crate) fn capturing(&self) -> bool {
        self.capture.is_some()
    }

    pub(crate) fn has_metadata(&self) -> bool {
        self.file.is_some() || self.last_written.is_some() || self.capture.is_some()
    }

    pub(crate) fn invalidate_capture(&mut self, path: Option<&Path>) {
        if let Some(capture) = &mut self.capture {
            if path.is_some_and(|path| {
                capture.temporary.as_deref() == Some(path)
                    || capture
                        .target
                        .as_ref()
                        .is_some_and(|target| target != path && target.starts_with(path))
            }) {
                return;
            }
            capture.failed = true;
        }
    }

    pub(crate) fn metadata_files(&self) -> Vec<(PathBuf, bool)> {
        let mut files: Vec<_> = self.file.iter().map(|file| (file.clone(), true)).collect();
        files.extend(self.last_written.iter().map(|file| (file.clone(), false)));
        if let Some(capture) = &self.capture {
            files.extend(
                capture
                    .target
                    .iter()
                    .chain(&capture.temporary)
                    .map(|file| (file.clone(), false)),
            );
        }
        files
    }

    pub(crate) fn filter(
        &mut self,
        uri: &str,
        diagnostics: &mut Vec<Diagnostic>,
        prepared: Prepared,
        ignored: &[String],
        publication: bool,
    ) -> Option<Option<usize>> {
        if self.epoch != prepared.epoch {
            return None;
        }
        let mut captured = Vec::new();
        let mut suppressed = 0;
        let mut index = 0;
        diagnostics.retain(|diagnostic| {
            let pair = prepared.hashes.as_ref().and_then(|pairs| pairs.get(index));
            index += 1;
            if crate::validate::code_is_suppressed(diagnostic.code.as_ref(), ignored) {
                return false;
            }
            if let Some((current, legacy)) = pair {
                if self.capture.is_some() {
                    captured.push(current.clone());
                }
                if baseline::matches(&self.status, current, legacy) {
                    suppressed += 1;
                    return false;
                }
            } else if let Some(capture) = &mut self.capture {
                capture.failed = true;
            }
            true
        });
        if let Some(capture) = &mut self.capture {
            if let Some(source) = prepared.source {
                capture.files.insert(uri.to_string(), (captured, source));
            } else if prepared.source_required {
                capture.failed = true;
            } else if index == 0 {
                capture.files.remove(uri);
            }
        }
        if !publication {
            return Some(None);
        }
        let previous = self.counts.remove(uri).unwrap_or(0);
        self.total = self.total - previous + suppressed;
        if suppressed != 0 {
            self.counts.insert(uri.to_string(), suppressed);
        }
        Some((previous != suppressed).then_some(self.total))
    }
}

impl crate::Backend {
    pub(crate) fn is_baseline_path(&self, path: &Path) -> bool {
        let files = self.state.baseline.lock().metadata_files();
        if !files
            .iter()
            .any(|(file, _)| file.file_name() == path.file_name())
        {
            return false;
        }
        let roots = self.state.config.read().editable_roots.clone();
        let resolved = crate::access::editable_target(path, &roots).ok();
        files
            .iter()
            .any(|(file, _)| resolved.as_ref() == Some(file))
    }

    pub(crate) async fn reload_baseline(&self, setting: String) -> bool {
        let root = self.state.config.read().workspace_roots.first().cloned();
        let file = root
            .as_ref()
            .and_then(|root| baseline::resolve_target(root, &setting).ok());
        let value = setting.clone();
        let loaded = tokio::task::spawn_blocking(move || match root {
            Some(root) => baseline::load(&root, Some(&value)),
            None if value.is_empty() => Ok(BaselineStatus::Disabled),
            None => Err(baseline::BaselineError::InvalidTarget(
                "no workspace root".to_string(),
            )),
        })
        .await;
        let (status, error) = match loaded {
            Ok(Ok(status)) => (status, None),
            Ok(Err(error)) => (BaselineStatus::Disabled, Some(error.message())),
            Err(error) => (BaselineStatus::Disabled, Some(error.to_string())),
        };
        let changed = {
            let _publication = self.state.diagnostic_publication_lock.lock().await;
            let mut state = self.state.baseline.lock();
            if state.setting == setting && *state.status == status && state.error == error {
                false
            } else {
                state.file = file;
                state.setting = setting;
                state.status = Arc::new(status);
                state.error = error.clone();
                state.epoch = state.epoch.wrapping_add(1);
                state.counts.clear();
                state.total = 0;
                if let Some(capture) = &mut state.capture {
                    capture.failed = true;
                }
                true
            }
        };
        if changed {
            self.client
                .send_notification::<StatusNotification>(serde_json::json!({"suppressed": 0}))
                .await;
            if let Some(error) = error {
                self.client.show_message(MessageType::WARNING, error).await;
            }
        }
        changed
    }

    pub(crate) async fn refresh_baseline_diagnostics(&self) {
        let rules = self.state.config.read().rules_dir.clone();
        if let Some(rules) = rules {
            self.load_rules_config(&rules).await;
        }
        if !self.validate_entire_workspace(false).await {
            self.spawn_deferred_revalidation("diagnostics baseline changed");
        }
    }

    pub(crate) async fn prepare_baseline(
        &self,
        uri: &Url,
        diagnostics: &[Diagnostic],
        version: Option<i32>,
        expected_hash: Option<u64>,
    ) -> Prepared {
        let (epoch, active, capturing) = {
            let state = self.state.baseline.lock();
            let capturing = state.capture.is_some();
            let active = capturing
                || matches!(&*state.status, BaselineStatus::Loaded { hashes, .. } if !hashes.is_empty());
            (state.epoch, active, capturing)
        };
        let mut prepared = Prepared {
            epoch,
            hashes: None,
            source: None,
            source_required: capturing && (expected_hash.is_some() || !diagnostics.is_empty()),
        };
        if !active || (diagnostics.is_empty() && !prepared.source_required) {
            return prepared;
        }
        let (root, roots) = {
            let cfg = self.state.config.read();
            let root = if uri.path().ends_with(".cwt") {
                cfg.rules_dir.clone()
            } else {
                cfg.workspace_roots.first().cloned()
            };
            (root, cfg.authorized_roots.clone())
        };
        let Some(root) = root else { return prepared };
        let root = std::fs::canonicalize(&root).unwrap_or(root);
        let Some(path) = crate::access::authorized_path(uri.as_str(), &roots) else {
            return prepared;
        };
        let open = {
            let documents = self.state.documents.lock();
            documents
                .get(uri.as_str())
                .map(|doc| (doc.text.clone(), doc.version))
        };
        let source_version = open.as_ref().map(|(_, version)| *version);
        let text = match open {
            Some((text, current)) if version == Some(current) => Some(text),
            Some(_) => None,
            None => {
                let path = path.clone();
                tokio::task::spawn_blocking(move || {
                    crate::access::read_capped_text(&path, crate::access::MAX_URI_READ_BYTES)
                        .map(Arc::<str>::from)
                })
                .await
                .ok()
                .flatten()
            }
        };
        let Some(text) = text else { return prepared };
        let hash = cwtools_cache::workspace::content_hash(&text);
        if expected_hash.is_some_and(|expected| expected != hash) {
            return prepared;
        }
        let lines: Vec<&str> = text.lines().collect();
        let key = cwtools_file_manager::path_identity(&path);
        prepared.hashes = Some(
            diagnostics
                .iter()
                .map(|diagnostic| {
                    let line = diagnostic
                        .data
                        .as_ref()
                        .and_then(|data| data.get("baselineLine"))
                        .and_then(serde_json::Value::as_u64)
                        .map_or(diagnostic.range.start.line + 1, |line| line as u32);
                    let source = line
                        .checked_sub(1)
                        .and_then(|line| lines.get(line as usize))
                        .map_or("", |line| line.trim());
                    let code = match diagnostic.code.as_ref() {
                        Some(tower_lsp::lsp_types::NumberOrString::String(code)) => code.as_str(),
                        _ => "",
                    };
                    (
                        cwtools_driver::diagnostic_hash::diag_hash_for_native_path(
                            &root,
                            &key,
                            &path,
                            code,
                            &diagnostic.message,
                            source,
                        ),
                        cwtools_driver::diagnostic_hash::legacy_diag_hash_for_native_path(
                            &root,
                            &key,
                            &path,
                            code,
                            &diagnostic.message,
                            line,
                        ),
                    )
                })
                .collect(),
        );
        prepared.source = Some(SourceSnapshot {
            path,
            hash,
            version: source_version,
        });
        prepared
    }

    pub(crate) async fn capture_baseline_only(
        &self,
        uri: &str,
        diagnostics: Vec<Diagnostic>,
        hash: Option<u64>,
    ) {
        let Ok(url) = Url::parse(uri) else { return };
        let prepared = self.prepare_baseline(&url, &diagnostics, None, hash).await;
        let _publication = self.state.diagnostic_publication_lock.lock().await;
        if self.state.documents.lock().contains_key(uri) {
            return;
        }
        let mut diagnostics = diagnostics;
        let codes = self.state.config.read().ignored_error_codes.clone();
        self.state
            .baseline
            .lock()
            .filter(uri, &mut diagnostics, prepared, &codes, false);
    }

    pub(crate) async fn write_baseline_command(
        &self,
        arguments: &[serde_json::Value],
        token: Option<ProgressToken>,
    ) -> Result<Option<serde_json::Value>> {
        let root = self
            .state
            .config
            .read()
            .workspace_roots
            .first()
            .cloned()
            .ok_or_else(|| failure("No workspace is open"))?;
        let setting = self.state.baseline.lock().setting.clone();
        if arguments.len() > 1 || arguments.first().is_some_and(|value| !value.is_string()) {
            return Err(Error::invalid_params(
                "Expected one optional workspace-relative baseline path",
            ));
        }
        let relative = arguments
            .first()
            .and_then(serde_json::Value::as_str)
            .unwrap_or(&setting);
        let target =
            baseline::resolve_target(&root, relative).map_err(|error| failure(error.message()))?;
        let check_root = root.clone();
        let check_setting = relative.to_string();
        tokio::task::spawn_blocking(move || baseline::load(&check_root, Some(&check_setting)))
            .await
            .map_err(failure)?
            .map_err(|error| {
                failure(format!(
                    "Refusing to overwrite an invalid baseline: {}",
                    error.message()
                ))
            })?;
        {
            let mut state = self.state.baseline.lock();
            if state.capture.is_some() || self.state.scan_in_progress.load(Ordering::Acquire) {
                return Err(failure(
                    "A workspace scan is in progress; retry writing the baseline when it finishes",
                ));
            }
            state.capture = Some(Capture {
                target: Some(target.clone()),
                ..Capture::default()
            });
        }
        let _capture = CaptureGuard(self.state.clone());
        let progress = crate::command_progress::CommandProgress::begin(
            self,
            token,
            "CWTools: Write diagnostics baseline",
            true,
        )
        .await;
        let result = self.collect_baseline(&root, &target, &progress).await;
        progress.finish(None).await;
        result
    }

    async fn collect_baseline(
        &self,
        root: &Path,
        target: &Path,
        progress: &crate::command_progress::CommandProgress,
    ) -> Result<Option<serde_json::Value>> {
        let edits = self.state.edit_generation.load(Ordering::Acquire);
        let roots = self
            .state
            .workspace_roots_generation
            .load(Ordering::Acquire);
        let settings = self.state.settings_generation.load(Ordering::Acquire);
        let rules = self.state.config.read().rules_dir.clone();
        let loaded = match rules {
            Some(rules) => self.load_rules_config(&rules).await,
            None => false,
        };
        let expected_settings = settings.wrapping_add(u64::from(loaded));
        let outcome = self
            .wait_for_scan(
                crate::scan::retry_deadline(std::time::Duration::from_secs(60)),
                Some(progress),
            )
            .await;
        if outcome != crate::command_progress::ScanOutcome::Ran || progress.is_cancelled() {
            return Err(failure(
                "Baseline capture did not finish; no baseline was written",
            ));
        }
        if self.state.edit_generation.load(Ordering::Acquire) != edits
            || self
                .state
                .workspace_roots_generation
                .load(Ordering::Acquire)
                != roots
            || self.state.settings_generation.load(Ordering::Acquire) != expected_settings
        {
            return Err(failure(
                "Workspace changed during baseline capture; retry without editing",
            ));
        }
        let (hashes, sources, failed) = {
            let state = self.state.baseline.lock();
            let capture = state.capture.as_ref().expect("capture guard owns state");
            let hashes: HashSet<String> = capture
                .files
                .values()
                .flat_map(|(hashes, _)| hashes.iter().cloned())
                .collect();
            let sources: Vec<_> = capture
                .files
                .iter()
                .map(|(uri, (_, source))| (uri.clone(), source.clone()))
                .collect();
            (hashes, sources, capture.failed)
        };
        if failed {
            return Err(failure(
                "Some diagnostic source snapshots were stale or unreadable; no baseline was written",
            ));
        }
        let text = baseline::render_hashes(hashes.iter().map(String::as_str));
        if text.len() as u64 > baseline::MAX_BASELINE_BYTES {
            return Err(failure("Diagnostics baseline exceeds the 16 MiB limit"));
        }
        let root = root.to_path_buf();
        let target = target.to_path_buf();
        let state = self.state.clone();
        let pending =
            tokio::task::spawn_blocking(move || -> std::result::Result<PendingBaseline, String> {
                for (uri, source) in sources {
                    let closed = {
                        let documents = state.documents.lock();
                        match (source.version, documents.get(&uri)) {
                            (Some(version), Some(document))
                                if document.version == version
                                    && documents.content_hash(&uri) == Some(source.hash) =>
                            {
                                false
                            }
                            (None, None) => true,
                            _ => return Err(format!("{uri} changed during capture; retry")),
                        }
                    };
                    if closed {
                        let current = crate::access::read_capped_text(
                            &source.path,
                            crate::access::MAX_URI_READ_BYTES,
                        )
                        .ok_or_else(|| format!("Could not verify {}", source.path.display()))?;
                        if cwtools_cache::workspace::content_hash(&current) != source.hash {
                            return Err(format!(
                                "{} changed during capture; retry",
                                source.path.display()
                            ));
                        }
                    }
                }
                let canonical = std::fs::canonicalize(&root).map_err(|error| error.to_string())?;
                let target = crate::access::editable_target(&target, &[canonical])
                    .map_err(|error| error.reason().to_string())?;
                let parent = target.parent().ok_or("Baseline has no parent directory")?;
                std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                let temporary =
                    parent.join(format!(".cwtools-baseline-{}.tmp", std::process::id()));
                if let Some(capture) = &mut state.baseline.lock().capture {
                    capture.temporary = Some(temporary.clone());
                }
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&temporary)
                    .map_err(|error| error.to_string())?;
                let pending = PendingBaseline { temporary, target };
                file.write_all(text.as_bytes())
                    .map_err(|error| error.to_string())?;
                file.sync_all().map_err(|error| error.to_string())?;
                Ok(pending)
            })
            .await
            .map_err(failure)?
            .map_err(failure)?;
        let _publication = self.state.diagnostic_publication_lock.lock().await;
        if progress.is_cancelled()
            || self.state.edit_generation.load(Ordering::Acquire) != edits
            || self
                .state
                .workspace_roots_generation
                .load(Ordering::Acquire)
                != roots
            || self.state.settings_generation.load(Ordering::Acquire) != expected_settings
            || self
                .state
                .baseline
                .lock()
                .capture
                .as_ref()
                .is_none_or(|capture| capture.failed)
        {
            return Err(failure(
                "Workspace changed or capture was cancelled; no baseline was written",
            ));
        }
        let written =
            tokio::task::spawn_blocking(move || -> std::result::Result<PathBuf, String> {
                let target = crate::access::editable_target(
                    &pending.target,
                    &[pending
                        .target
                        .parent()
                        .ok_or("Baseline has no parent directory")?
                        .to_path_buf()],
                )
                .map_err(|error| error.reason().to_string())?;
                if target != pending.target {
                    return Err("Baseline target changed during capture".to_string());
                }
                std::fs::rename(&pending.temporary, &target).map_err(|error| error.to_string())?;
                Ok(target)
            })
            .await
            .map_err(failure)?
            .map_err(failure)?;
        self.state.baseline.lock().last_written = Some(written.clone());
        Ok(Some(
            serde_json::json!({"file": written, "hashes": hashes.len()}),
        ))
    }
}
