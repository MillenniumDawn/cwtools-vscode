use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use serde_json::{Map, Value, json};

use cwtools_info::vanilla_cache::{self, CacheStatus};
use cwtools_rules::rules_types::RuleSet;

use crate::Backend;
use crate::paths::discover_vanilla_dir;
use crate::scan::vanilla_fingerprint;

fn path_strings(paths: &[PathBuf]) -> Vec<String> {
    paths.iter().map(|p| p.display().to_string()).collect()
}

fn cache_report(path: &Path, fingerprint: &str, status: CacheStatus) -> Map<String, Value> {
    let mut cache = Map::new();
    cache.insert("path".into(), json!(path.display().to_string()));
    cache.insert("fingerprint".into(), json!(fingerprint));
    let label = match status {
        CacheStatus::Present => "present",
        CacheStatus::Absent => "absent",
        CacheStatus::Stale { others } => {
            cache.insert("otherCaches".into(), json!(others));
            "stale"
        }
        CacheStatus::VersionMismatch { found, expected } => {
            cache.insert("fileVersion".into(), json!(found));
            cache.insert("expectedVersion".into(), json!(expected));
            "versionMismatch"
        }
        CacheStatus::Unreadable(reason) => {
            cache.insert("reason".into(), json!(reason));
            "unreadable"
        }
    };
    cache.insert("status".into(), json!(label));
    cache
}

fn unavailable(reason: &str) -> Value {
    json!({ "status": "unavailable", "reason": reason })
}

impl Backend {
    /// The state a support request needs, read when asked for. Nothing is recorded on
    /// the scan, validation or edit paths. The rules git HEAD is left to the client.
    pub(crate) fn get_setup_report(&self) -> Value {
        let (language, game_known, workspace, ignore, vanilla_dir, rules_dir, shadow) = {
            let cfg = self.state.config.read();
            (
                cfg.language.clone(),
                cfg.game().is_some(),
                json!({
                    "roots": path_strings(&cfg.workspace_roots),
                    "parentMods": path_strings(&cfg.parent_roots),
                }),
                json!({
                    "filePatterns": cfg.ignore_file_patterns,
                    "directories": cfg.ignore_dir_patterns,
                    "errorCodes": cfg.ignored_error_codes,
                }),
                cfg.vanilla_dir.clone(),
                cfg.rules_dir.clone(),
                cfg.workspace_roots.first().map(|primary| {
                    cwtools_driver::vanilla_replacement_shadow(primary, &cfg.parent_roots)
                }),
            )
        };
        let ruleset = self.state.rules.read().ruleset.clone();
        let rules = json!({
            "dir": rules_dir.as_ref().map(|dir| dir.display().to_string()),
            "dirExists": rules_dir.as_deref().is_some_and(Path::is_dir),
            "loaded": ruleset.is_some(),
            "types": ruleset.as_ref().map_or(0, |r| r.types.len()),
        });
        let vanilla =
            self.vanilla_report(&language, vanilla_dir, ruleset.as_deref(), shadow.as_ref());

        let index = {
            let info = self.state.info_service.read();
            let map = info.type_index.map();
            json!({
                "typeInstances": map.values().map(Vec::len).sum::<usize>(),
                "types": map.values().filter(|v| !v.is_empty()).count(),
                "files": info.type_index.file_index.len(),
                "variables": info.type_index.var_index.len(),
            })
        };
        let loc = self.state.loc_index.read().clone();
        let localisation = json!({
            "keys": loc.as_ref().map_or(0, |l| l.union().len()),
            "languages": loc.as_ref().map_or(0, |l| l.languages_with_data().len()),
        });
        let last_scan = self
            .state
            .last_scan_summary
            .lock()
            .as_ref()
            .map(crate::scan::ScanSummary::to_json);

        json!({
            "server": { "version": env!("CARGO_PKG_VERSION") },
            "state": {
                "indexReady": self.state.index_ready.load(Ordering::Relaxed),
                "scanInProgress": self.state.scan_in_progress.load(Ordering::Relaxed),
            },
            "game": { "language": language, "known": game_known },
            "workspace": workspace,
            "rules": rules,
            "vanilla": vanilla,
            "index": index,
            "localisation": localisation,
            "lastScan": last_scan,
            "ignore": ignore,
        })
    }

    fn vanilla_report(
        &self,
        language: &str,
        configured: Option<PathBuf>,
        ruleset: Option<&RuleSet>,
        shadow: Option<&cwtools_file_manager::file_manager::LayerShadow>,
    ) -> Value {
        let dir = configured.or_else(|| discover_vanilla_dir(language));
        let dir_exists = dir.as_deref().is_some_and(Path::is_dir);
        let cache = match (&dir, ruleset) {
            (None, _) => unavailable("no base-game directory is set or found"),
            (Some(_), _) if !dir_exists => unavailable("the base-game directory does not exist"),
            (_, None) => unavailable("no rules are loaded, so the cache fingerprint is unknown"),
            (Some(dir), Some(ruleset)) => {
                let fingerprint = vanilla_fingerprint(dir, ruleset, shadow);
                match self.vanilla_cache_path(language, &fingerprint) {
                    None => unavailable("no cache directory is available"),
                    Some(path) => Value::Object(cache_report(
                        &path,
                        &fingerprint,
                        vanilla_cache::cache_status(&path, language),
                    )),
                }
            }
        };
        json!({
            "dir": dir.as_ref().map(|dir| dir.display().to_string()),
            "dirExists": dir_exists,
            "merged": self.state.vanilla_merged.load(Ordering::SeqCst),
            "cache": cache,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use cwtools_info::vanilla_cache::VanillaCacheAux;

    use crate::testing::test_backend;

    fn cache_status_of(report: &Value) -> &str {
        report["vanilla"]["cache"]["status"]
            .as_str()
            .unwrap_or_else(|| panic!("no cache status in {report}"))
    }

    fn backend_with_install() -> (crate::Backend, tempfile::TempDir) {
        let backend = test_backend();
        let tmp = tempfile::tempdir().unwrap();
        let install = tmp.path().join("install");
        std::fs::create_dir_all(&install).unwrap();
        {
            let mut config = backend.state.config.write();
            config.language = "hoi4".into();
            config.vanilla_dir = Some(install);
            config.cache_dir = Some(tmp.path().join("cache"));
        }
        backend.set_ruleset(RuleSet::new());
        (backend, tmp)
    }

    fn expected_cache_path(backend: &crate::Backend, tmp: &tempfile::TempDir) -> PathBuf {
        let fingerprint = vanilla_fingerprint(&tmp.path().join("install"), &RuleSet::new(), None);
        backend
            .vanilla_cache_path("hoi4", &fingerprint)
            .expect("configured cache directory")
    }

    fn write_cache(path: &Path, fingerprint: &str) {
        vanilla_cache::save_per_type(
            &std::collections::HashMap::new(),
            "hoi4",
            fingerprint,
            path,
            VanillaCacheAux::default(),
        )
        .unwrap();
    }

    #[test]
    fn report_distinguishes_absent_stale_version_mismatched_and_present_caches() {
        let (backend, tmp) = backend_with_install();
        let path = expected_cache_path(&backend, &tmp);
        let cache_dir = path.parent().unwrap().to_path_buf();

        assert_eq!(cache_status_of(&backend.get_setup_report()), "absent");

        write_cache(
            &cache_dir.join("vanilla-hoi4-another-install.cwv"),
            "another",
        );
        let report = backend.get_setup_report();
        assert_eq!(cache_status_of(&report), "stale");
        assert_eq!(report["vanilla"]["cache"]["otherCaches"], 1);

        write_cache(&path, "current");
        let report = backend.get_setup_report();
        assert_eq!(cache_status_of(&report), "present");
        assert_eq!(
            report["vanilla"]["cache"]["path"],
            path.display().to_string()
        );

        let mut bytes = std::fs::read(&path).unwrap();
        bytes[4] = 0;
        std::fs::write(&path, &bytes).unwrap();
        let report = backend.get_setup_report();
        assert_eq!(cache_status_of(&report), "versionMismatch");
        assert_eq!(report["vanilla"]["cache"]["fileVersion"], 0);
        assert_ne!(report["vanilla"]["cache"]["expectedVersion"], 0);
    }

    #[test]
    fn report_says_why_the_cache_state_is_unavailable() {
        let backend = test_backend();
        backend.state.config.write().language = "notagame".into();
        let report = backend.get_setup_report();
        assert_eq!(cache_status_of(&report), "unavailable");
        assert_eq!(report["vanilla"]["dir"], Value::Null);

        let tmp = tempfile::tempdir().unwrap();
        backend.state.config.write().vanilla_dir = Some(tmp.path().join("gone"));
        let report = backend.get_setup_report();
        assert_eq!(report["vanilla"]["dirExists"], false);
        assert!(
            report["vanilla"]["cache"]["reason"]
                .as_str()
                .is_some_and(|r| r.contains("does not exist")),
            "{report}"
        );

        backend.state.config.write().vanilla_dir = Some(tmp.path().to_path_buf());
        let report = backend.get_setup_report();
        assert!(
            report["vanilla"]["cache"]["reason"]
                .as_str()
                .is_some_and(|r| r.contains("no rules")),
            "{report}"
        );
    }

    #[test]
    fn report_flags_a_rules_dir_that_does_not_exist() {
        let backend = test_backend();
        let tmp = tempfile::tempdir().unwrap();
        let gone = tmp.path().join("no-such-rules");
        backend.state.config.write().rules_dir = Some(gone.clone());

        let report = backend.get_setup_report();

        assert_eq!(report["rules"]["dir"], gone.display().to_string());
        assert_eq!(report["rules"]["dirExists"], false);
        assert_eq!(report["rules"]["loaded"], false);
    }

    #[test]
    fn report_carries_config_scan_and_ignore_state() {
        let backend = test_backend();
        let tmp = tempfile::tempdir().unwrap();
        {
            let mut config = backend.state.config.write();
            config.language = "hoi4".into();
            config.workspace_roots = vec![tmp.path().to_path_buf()];
            config.ignore_file_patterns = vec!["*.bak".into()];
            config.ignore_dir_patterns = vec!["scratch".into()];
            config.ignored_error_codes = vec!["cw100".into()];
        }
        assert_eq!(backend.get_setup_report()["lastScan"], Value::Null);
        *backend.state.last_scan_summary.lock() = Some(crate::scan::ScanSummary {
            total_files: 9,
            validated_files: 8,
            ..Default::default()
        });

        let report = backend.get_setup_report();

        assert_eq!(report["server"]["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(report["game"], json!({ "language": "hoi4", "known": true }));
        assert_eq!(
            report["workspace"]["roots"],
            json!([tmp.path().display().to_string()])
        );
        assert_eq!(report["lastScan"]["totalFiles"], 9);
        assert_eq!(report["lastScan"]["validatedFiles"], 8);
        assert_eq!(
            report["ignore"],
            json!({
                "filePatterns": ["*.bak"],
                "directories": ["scratch"],
                "errorCodes": ["cw100"],
            })
        );
        assert_eq!(report["index"]["typeInstances"], 0);
    }
}
