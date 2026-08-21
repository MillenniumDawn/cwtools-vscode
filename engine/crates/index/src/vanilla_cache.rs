//! Pre-generated cache of base-game ("vanilla") data.
//!
//! Parsing and indexing a full game install on every run is slow, so the
//! vanilla data is built once and serialized here. Loading it resolves
//! references into base-game content (sprites, operation_tokens, equipment, …)
//! without re-parsing, and without validating vanilla files (which carry known
//! base-game errors we never want to report). Shared by the CLI
//! (`cache-vanilla` / `validate --vanilla-cache`) and the LSP server.
//!
//! Besides the type instances the cache also carries the vanilla loc-key sets
//! (per language), the vanilla file-path set (for CW113 `filepath` checks) and
//! the vanilla script-variable names, so a cache hit skips walking the install
//! for loc and file indexing too. Vanilla loc *entries* (command chains) are
//! NOT cached: the only consumer is the scope-aware command check on vanilla's
//! own content, which we never validate.

// zstd level, the atomic write and the bounded read/decode used by the `.cwb`
// parse cache too.
use cwtools_cache::io::{ZSTD_LEVEL, decode_capped, read_capped, write_atomically};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;

use cwtools_rules::rules_types::{RuleSet, SkipRootKey};

use crate::{SourceLocation, TypeIndex, TypeInstance};

/// Magic bytes at the start of every vanilla cache file. Distinct from the
/// `.cwb` parse cache magic (`CWB\0`) so the two can never be confused.
const MAGIC: &[u8; 4] = b"CWV\x00";

/// Filename prefix and extension of a cache file, as [`cache_file_name`] builds
/// it and [`is_cache_file`] recognises it.
const FILE_PREFIX: &str = "vanilla-";
const FILE_EXT: &str = ".cwv";

// v2 adds `fingerprint` (game version) so a cache can be validated against the
// installed game and shared between users on the same version. v1 files fail the
// version check and are treated as a cache miss (rebuilt).
// v3 folds the ruleset shape into the fingerprint (see `combined_fingerprint`):
// the cached instances are extracted *by the .cwt rules*, so a rules change makes
// a same-game-version cache stale. v2 files fail the version check (rebuilt).
// v4 switches the on-disk format from JSON to magic+version-framed zstd(rkyv)
// and adds loc keys, file paths, and variable names. Older JSON files fail the
// magic check and are treated as a cache miss (rebuilt).
// v5 adds complex-enum members and value_set members (completion data).
// v6 adds subtype-qualified membership keys (`type.subtype`) to the cached
// instances so `<type.subtype>` references into base-game content resolve. v5
// caches lack them, so they must rebuild (else e.g. naval equipment variants
// referencing a vanilla archetype lose their subtype).
// v7 carries the per-instance source file (`CachedInstance.f`) through `load`
// into `per_type` so goto-definition / find-references into base-game content
// land in the real vanilla file. The LSP's own writer (`save_per_type`) left
// `f` blank in v6, so those caches must rebuild to gain the source paths.
// v8 adds the definition's end position (`CachedInstance.el`/`ec`) so a cached
// vanilla instance carries its full extent (`SourceLocation.end`), matching
// live-scanned instances. v7 files lack the end fields, so the rkyv layout
// differs and they must rebuild.
// v9 folds `value_set[array]` names into the cached variable names (arrays are
// variables to the engine, so `add_to_array` defines a name CW246 must accept).
// v8 caches were written without them and would flag every vanilla array read.
// v10 stores the cached `file_paths` in their original on-disk case (was
// lowercased), so a case-sensitive run (--case-sensitive-files) can enforce
// exact case against base-game files too. v9 caches, whose paths were
// lowercased, must rebuild or a case-sensitive run would flag every vanilla
// reference.
// v11 adds `CachedInstance.p`, the explicit-field primary loc key (e.g. an
// event's `title`), so cached-path hover shows the same localised title as a
// live vanilla scan instead of falling back to a name-derived key. v10 caches
// lack it and restore it as `None` (#141).
// v12 adds `scripted_loc_names`, the base game's scripted-localisation names, so
// a cached run can tell a loc command naming one from a typo. v11 caches lack
// them, and a mod using a vanilla scripted localisation would flag CW226/CW266
// on every use (#348).
// v13 adds `scripted_gui_names`, the base game's scripted-GUI callback names,
// so `[!name]` calls can resolve callbacks supplied by vanilla (#350).
const CACHE_VERSION: u8 = 13;

/// Hard caps on a `.cwv` read and on what its body may decompress to. The path
/// comes from `--vanilla-cache` or an LSP client's `vanillaCache` (#162), so a
/// few kilobytes of `CWV`-prefixed zstd must not be able to expand until the
/// process dies. A cache of a full HOI4 install is 20 MiB compressed / 81 MiB
/// decoded, so these leave several times that; going over reads as a miss and
/// the install is re-indexed.
const MAX_CACHE_FILE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_CACHE_DECODED_BYTES: u64 = 512 * 1024 * 1024;

#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
struct CachedInstance {
    /// type name
    t: String,
    /// instance name
    n: String,
    /// source file (the instance's real path, so goto-into-vanilla resolves)
    f: String,
    /// start line
    l: u32,
    /// start column
    c: u16,
    /// end line
    el: u32,
    /// end column
    ec: u16,
    /// explicit-field primary loc key (e.g. an event's `title`), if any
    p: Option<String>,
}

#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
struct VanillaCacheFile {
    game: String,
    /// Game-version fingerprint (see [`fingerprint`]). A cache is valid only for
    /// the install that produced this fingerprint.
    fingerprint: String,
    instances: Vec<CachedInstance>,
    /// language name (`english`, `simp_chinese`, …) -> lowercased loc keys.
    loc_keys: Vec<(String, Vec<String>)>,
    /// Relative paths of every file under the install, in original on-disk
    /// case (forward slashes). Lowercased into the file index on restore.
    file_paths: Vec<String>,
    /// Script-variable names defined in vanilla (`VarIndex` form).
    var_names: Vec<String>,
    /// Complex-enum members extracted from vanilla files (enum name -> values).
    complex_enum_values: Vec<(String, Vec<String>)>,
    /// `value_set[...]` members written by vanilla files (namespace -> values).
    value_set_values: Vec<(String, Vec<String>)>,
    /// Scripted-localisation names defined in vanilla (`ScriptedLocIndex` form).
    scripted_loc_names: Vec<String>,
    /// Scripted-GUI callback names defined in vanilla (`ScriptedGuiIndex` form).
    scripted_gui_names: Vec<String>,
}

/// The non-instance half of the cache payload. Built by whoever walks the
/// install (CLI `cache-vanilla`, the stale-rebuild paths) and stored alongside
/// the type instances.
///
/// The same type comes back out of [`load`] (as [`VanillaCacheData::aux`]), so
/// a field one side packs is a field the other side has to route somewhere: the
/// editor cannot quietly restore a subset of what the CLI wrote (#283).
#[derive(Clone, Debug, Default)]
pub struct VanillaCacheAux {
    /// language name -> lowercased loc keys
    pub loc_keys: Vec<(String, Vec<String>)>,
    /// relative paths in original on-disk case (forward slashes)
    pub file_paths: Vec<String>,
    /// script-variable names
    pub var_names: Vec<String>,
    /// complex-enum members (enum name -> values)
    pub complex_enum_values: Vec<(String, Vec<String>)>,
    /// `value_set[...]` members (namespace -> values)
    pub value_set_values: Vec<(String, Vec<String>)>,
    /// scripted-localisation names
    pub scripted_loc_names: Vec<String>,
    /// scripted-GUI callback names
    pub scripted_gui_names: Vec<String>,
}

/// Everything a loaded cache provides, ready to merge into a session.
#[derive(Debug)]
pub struct VanillaCacheData {
    /// type name -> instances, each paired with its real source file (raw path,
    /// the driver / `TypeIndex.map` form). Consumers that navigate convert the
    /// path to a `file://` URI when merging into the live index.
    pub per_type: HashMap<String, Vec<(Arc<str>, TypeInstance)>>,
    /// Everything else the writer packed, in the type it packed it as.
    pub aux: VanillaCacheAux,
}

/// A stable fingerprint of a base-game install, used to invalidate the cache
/// when the game updates. Prefers the Paradox launcher's `rawVersion` (portable:
/// the same across every user on that version, so a built cache can be shared),
/// and falls back to the install directory's mtime when no version file exists.
pub fn fingerprint(dir: &Path) -> String {
    let launcher = dir.join("launcher-settings.json");
    if let Ok(text) = std::fs::read_to_string(&launcher)
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(&text)
    {
        if let Some(ver) = v.get("rawVersion").and_then(|x| x.as_str()) {
            return format!("v{ver}");
        }
        if let Some(ver) = v.get("version").and_then(|x| x.as_str()) {
            return format!("ver-{ver}");
        }
    }
    if let Ok(meta) = std::fs::metadata(dir)
        && let Ok(mtime) = meta.modified()
        && let Ok(dur) = mtime.duration_since(std::time::UNIX_EPOCH)
    {
        return format!("mtime-{}", dur.as_secs());
    }
    // No version file and no readable mtime: hash the install path so two
    // different unreadable installs don't collide on one "unknown" cache key.
    let h = fnv1a(dir.to_string_lossy().as_bytes(), 0xcbf2_9ce4_8422_2325u64);
    format!("unknown-{h:016x}")
}

/// FNV-1a over `bytes`, continuing from `hash`. A stable, dependency-free hash
/// (unlike `std::hash::DefaultHasher`, whose output isn't guaranteed across Rust
/// versions) so a cache fingerprint stays comparable across restarts/toolchains.
fn fnv1a(bytes: &[u8], mut hash: u64) -> u64 {
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// A stable hash of the parts of the ruleset that decide *which* vanilla type
/// instances get extracted and under *what name* (`collect_type_instances`):
/// type name, paths, `name_field`, `skip_root_key`, `starts_with`,
/// `type_per_file`, `key_prefix`, `type_key_filter`, `unique`, and subtype
/// key fields. When these change, a cache built from the old rules is stale even
/// if the game version is identical, so this is folded into the fingerprint.
pub(crate) fn ruleset_shape_hash(ruleset: &RuleSet) -> String {
    let skip_str = |s: &SkipRootKey| match s {
        SkipRootKey::SpecificKey(k) => format!("s:{k}"),
        SkipRootKey::AnyKey => "any".to_string(),
        SkipRootKey::MultipleKeys(ks, mk) => format!("m:{}:{}", ks.join(","), mk.is_equals()),
    };
    let mut parts: Vec<String> = ruleset
        .types
        .iter()
        .map(|t| {
            let mut paths = t.path_options.paths.clone();
            paths.sort();
            let skip = t.skip_root_key.iter().map(skip_str).collect::<Vec<_>>();
            let mut subs = t
                .subtypes
                .iter()
                .map(|s| {
                    format!(
                        "{}|{}|{:?}|{}",
                        s.name,
                        s.type_key_field.as_deref().unwrap_or(""),
                        s.type_key_filter,
                        s.starts_with.as_deref().unwrap_or(""),
                    )
                })
                .collect::<Vec<_>>();
            subs.sort();
            format!(
                "{}|nf={}|paths={}|skip={}|sw={}|tpf={}|kp={}|tkf={:?}|uniq={}|subs={}",
                t.name,
                t.name_field.as_deref().unwrap_or(""),
                paths.join(","),
                skip.join(","),
                t.starts_with.as_deref().unwrap_or(""),
                t.type_per_file,
                t.key_prefix.as_deref().unwrap_or(""),
                t.type_key_filter,
                t.unique,
                subs.join(";"),
            )
        })
        .collect();
    parts.sort();
    // The config's folders.cwt scopes which subdirectories get indexed, so a
    // folder-list change must invalidate the cache like any type-shape change.
    if !ruleset.folders.is_empty() {
        let mut folders = ruleset.folders.clone();
        folders.sort();
        parts.push(format!("folders={}", folders.join(",")));
    }
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for p in &parts {
        h = fnv1a(p.as_bytes(), h);
        h = fnv1a(b"\x1e", h); // record separator so concatenation is unambiguous
    }
    format!("{h:016x}")
}

/// The fingerprint a cache should be keyed by: the game-version fingerprint
/// ([`fingerprint`]) combined with the ruleset-shape hash ([`ruleset_shape_hash`]).
/// Use this for both [`save`] and the freshness comparison on [`load`].
pub fn combined_fingerprint(dir: &Path, ruleset: &RuleSet) -> String {
    format!("{}|rs:{}", fingerprint(dir), ruleset_shape_hash(ruleset))
}

/// Filename of the cache for `game` at `fingerprint`, versioned in the name so
/// several game versions coexist in one directory.
///
/// One builder, so a writer (the CLI's or the LSP's) and [`is_cache_file`] can
/// never disagree about what a cache file is called. Both halves are sanitised:
/// a fingerprint carries the game version verbatim, and a separator in it would
/// otherwise write the cache somewhere else entirely.
pub fn cache_file_name(game: &str, fingerprint: &str) -> String {
    let safe = |s: &str| -> String {
        s.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    };
    format!(
        "{FILE_PREFIX}{}-{}{FILE_EXT}",
        safe(game),
        safe(fingerprint)
    )
}

/// Whether `path` is one of the cache files this module writes: the
/// [`cache_file_name`] shape, a regular file rather than a symlink or a
/// directory, carrying the [`MAGIC`] header.
///
/// Reach for this before deleting one. `clearAllCaches` clears caches out of a
/// directory an LSP client chose (#159), so it has to recognise a cache by what
/// is in it and not by its name. A `.cwv.tmp…` left behind by a killed writer
/// matches too — same name, same header — but one truncated before its header
/// was written does not, and is left for its owner.
pub fn is_cache_file(path: &Path) -> bool {
    let named_like_a_cache = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(FILE_PREFIX) && name.contains(FILE_EXT));
    if !named_like_a_cache
        || !std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_file())
    {
        return false;
    }
    let mut header = [0u8; MAGIC.len()];
    std::fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut header))
        .is_ok()
        && &header == MAGIC
}

fn write_cache(
    instances: Vec<CachedInstance>,
    game: &str,
    fingerprint: &str,
    path: &Path,
    aux: VanillaCacheAux,
) -> std::io::Result<usize> {
    let count = instances.len();
    let cache = VanillaCacheFile {
        game: game.to_string(),
        fingerprint: fingerprint.to_string(),
        instances,
        loc_keys: aux.loc_keys,
        file_paths: aux.file_paths,
        var_names: aux.var_names,
        complex_enum_values: aux.complex_enum_values,
        value_set_values: aux.value_set_values,
        scripted_loc_names: aux.scripted_loc_names,
        scripted_gui_names: aux.scripted_gui_names,
    };
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&cache).map_err(std::io::Error::other)?;

    // Frame checksum on, as `.cwb` does. rkyv validates structure, not content,
    // so a flipped byte can decompress into a different-but-valid archive and be
    // served as a cache hit. The checksum turns that into a decode error, which
    // every consumer already degrades to a re-index. Readers need no change: a
    // frame without one still decodes, so old `.cwv` files stay loadable and
    // CACHE_VERSION doesn't move.
    let compressed = {
        let mut encoder = zstd::stream::Encoder::new(Vec::new(), ZSTD_LEVEL)?;
        encoder.include_checksum(true)?;
        encoder.write_all(&bytes)?;
        encoder.finish()?
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Temp-and-rename, as `.cwb` does. Writing onto the destination meant a
    // crash, a full disk or a second writer on the same path (the LSP and the
    // CLI share a cache dir and key the file by game + fingerprint) destroyed
    // the cache that was already there, costing a full re-index.
    write_atomically(path, |file| {
        file.write_all(MAGIC)?;
        file.write_all(&[CACHE_VERSION])?;
        file.write_all(&compressed)
    })?;
    Ok(count)
}

/// Serialize a vanilla type index (plus aux data) to `path`. Returns the
/// instance count written. `fingerprint` ties the cache to a specific game
/// version (see [`fingerprint`]).
pub fn save(
    index: &TypeIndex,
    game: &str,
    fingerprint: &str,
    path: &Path,
    aux: VanillaCacheAux,
) -> std::io::Result<usize> {
    let instances = index
        .map
        .iter()
        .flat_map(|(type_name, entries)| {
            entries.iter().map(move |(file_uri, inst)| CachedInstance {
                t: type_name.clone(),
                n: inst.name.clone(),
                f: file_uri.to_string(),
                l: inst.location.line,
                c: inst.location.col,
                el: inst.location.end.0,
                ec: inst.location.end.1,
                p: inst.primary_loc_key.clone(),
            })
        })
        .collect();
    write_cache(instances, game, fingerprint, path, aux)
}

/// As [`save`], but from a per-type instance map (the form the LSP keeps its
/// vanilla index in). Each instance's source file is preserved so a cache round
/// trip keeps goto-into-vanilla working.
pub fn save_per_type(
    per_type: &HashMap<String, Vec<(Arc<str>, TypeInstance)>>,
    game: &str,
    fingerprint: &str,
    path: &Path,
    aux: VanillaCacheAux,
) -> std::io::Result<usize> {
    let instances = per_type
        .iter()
        .flat_map(|(type_name, insts)| {
            insts.iter().map(move |(file_uri, inst)| CachedInstance {
                t: type_name.clone(),
                n: inst.name.clone(),
                f: file_uri.to_string(),
                l: inst.location.line,
                c: inst.location.col,
                el: inst.location.end.0,
                ec: inst.location.end.1,
                p: inst.primary_loc_key.clone(),
            })
        })
        .collect();
    write_cache(instances, game, fingerprint, path, aux)
}

/// Load a vanilla cache file. Returns `(game, fingerprint, data)`; the caller
/// compares `fingerprint` against the live install to decide whether it is
/// fresh. Old JSON caches (pre-v4) fail the magic check and read as a miss.
#[tracing::instrument(skip_all, fields(path = %path.display()))]
pub fn load(path: &Path) -> std::io::Result<(String, String, VanillaCacheData)> {
    let data = read_capped(path, MAX_CACHE_FILE_BYTES)?;
    if data.len() < MAGIC.len() + 1 || &data[..MAGIC.len()] != MAGIC {
        return Err(std::io::Error::other(
            "not a vanilla cache file (old JSON format or wrong file); rebuild with cache-vanilla",
        ));
    }
    if data[MAGIC.len()] != CACHE_VERSION {
        return Err(std::io::Error::other(format!(
            "vanilla cache version {} unsupported (expected {})",
            data[MAGIC.len()],
            CACHE_VERSION
        )));
    }
    let mut bytes = Vec::new();
    decode_capped(
        &data[MAGIC.len() + 1..],
        MAX_CACHE_DECODED_BYTES,
        &mut bytes,
    )?;
    let cache: VanillaCacheFile = rkyv::from_bytes::<VanillaCacheFile, rkyv::rancor::Error>(&bytes)
        .map_err(std::io::Error::other)?;
    let mut per_type: HashMap<String, Vec<(Arc<str>, TypeInstance)>> = HashMap::new();
    let mut file_uris: HashMap<String, Arc<str>> = HashMap::new();
    for ci in cache.instances {
        let file_uri = file_uris
            .entry(ci.f)
            .or_insert_with_key(|path| Arc::from(path.as_str()));
        per_type.entry(ci.t).or_default().push((
            Arc::clone(file_uri),
            TypeInstance {
                name: ci.n,
                location: SourceLocation {
                    line: ci.l,
                    col: ci.c,
                    end: (ci.el, ci.ec),
                },
                primary_loc_key: ci.p,
                required_loc_keys: Vec::new(),
            },
        ));
    }
    Ok((
        cache.game,
        cache.fingerprint,
        VanillaCacheData {
            per_type,
            aux: VanillaCacheAux {
                loc_keys: cache.loc_keys,
                file_paths: cache.file_paths,
                var_names: cache.var_names,
                complex_enum_values: cache.complex_enum_values,
                value_set_values: cache.value_set_values,
                scripted_loc_names: cache.scripted_loc_names,
                scripted_gui_names: cache.scripted_gui_names,
            },
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_cache_file_accepts_what_the_writer_wrote() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(cache_file_name("hoi4", "v1.16.4"));
        let empty = HashMap::new();
        save_per_type(&empty, "hoi4", "v1.16.4", &path, VanillaCacheAux::default()).unwrap();

        assert!(is_cache_file(&path));
    }

    #[test]
    fn is_cache_file_rejects_a_lookalike_name() {
        // `clearAllCaches` deletes what this says yes to, out of a directory an
        // LSP client chose (#159), so the name alone is never enough.
        let tmp = tempfile::tempdir().unwrap();
        let named_right = tmp.path().join("vanilla-holiday.cwv");
        std::fs::write(&named_right, b"JPEG, not a cache").unwrap();
        let named_wrong = tmp.path().join("vanilla-notes.txt");
        std::fs::write(&named_wrong, MAGIC).unwrap();
        let empty = tmp.path().join("vanilla-truncated.cwv");
        std::fs::write(&empty, b"").unwrap();
        let directory = tmp.path().join("vanilla-archive.cwv");
        std::fs::create_dir(&directory).unwrap();

        assert!(!is_cache_file(&named_right));
        assert!(!is_cache_file(&named_wrong));
        assert!(!is_cache_file(&empty));
        assert!(!is_cache_file(&directory));
    }

    #[cfg(unix)]
    #[test]
    fn is_cache_file_rejects_a_symlink_to_one() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join(cache_file_name("hoi4", "v1"));
        let empty = HashMap::new();
        save_per_type(&empty, "hoi4", "v1", &target, VanillaCacheAux::default()).unwrap();
        let link = tmp.path().join(cache_file_name("hoi4", "v2"));
        std::os::unix::fs::symlink(&target, &link).unwrap();

        assert!(!is_cache_file(&link));
    }

    #[test]
    fn a_save_that_cannot_land_leaves_no_temp_behind() {
        // `save` writes through a temp file and renames (so a killed writer
        // cannot destroy the cache that was there). The temp has to be cleaned
        // up when the rename cannot happen, or a cache dir accumulates one
        // `.cwv.tmp…` per failed run — which `is_cache_file` matches and
        // `clearAllCaches` would then be responsible for.
        let tmp = tempfile::tempdir().unwrap();
        let occupied = tmp.path().join(cache_file_name("hoi4", "v1.16.4"));
        std::fs::create_dir(&occupied).unwrap();

        let empty = HashMap::new();
        save_per_type(
            &empty,
            "hoi4",
            "v1.16.4",
            &occupied,
            VanillaCacheAux::default(),
        )
        .expect_err("a save onto a directory cannot succeed");

        let strays: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .flatten()
            .filter(|e| e.path() != occupied)
            .map(|e| e.file_name())
            .collect();
        assert!(strays.is_empty(), "temp file left behind: {strays:?}");
    }

    #[test]
    fn cache_file_name_sanitises_both_halves() {
        assert_eq!(
            cache_file_name("hoi4", "v1.16.4-rs:9ab/c"),
            "vanilla-hoi4-v1.16.4-rs_9ab_c.cwv"
        );
    }

    #[test]
    fn fingerprint_distinguishes_unreadable_installs() {
        // Two installs with no launcher file and no readable mtime must not
        // collide on a single "unknown" cache key (#9).
        let a = fingerprint(Path::new("/nonexistent/install/alpha"));
        let b = fingerprint(Path::new("/nonexistent/install/beta"));
        assert_ne!(
            a, b,
            "distinct unreadable installs need distinct fingerprints"
        );
        assert_ne!(a, "unknown");
    }

    #[test]
    fn round_trip_preserves_instances() {
        let mut idx = TypeIndex::new();
        let mut per: HashMap<String, Vec<TypeInstance>> = HashMap::new();
        per.insert(
            "spriteType".to_string(),
            vec![
                TypeInstance {
                    name: "GFX_a".into(),
                    location: SourceLocation {
                        line: 2,
                        col: 1,
                        end: (4, 1),
                    },
                    primary_loc_key: Some("GFX_A_TITLE".into()),
                    required_loc_keys: Vec::new(),
                },
                TypeInstance {
                    name: "GFX_b".into(),
                    location: SourceLocation {
                        line: 5,
                        col: 3,
                        end: (9, 4),
                    },
                    primary_loc_key: None,
                    required_loc_keys: Vec::new(),
                },
            ],
        );
        idx.merge("vanilla/x.gfx", per);

        let dir = std::env::temp_dir();
        let path = dir.join("cwtools_vanilla_cache_test.cwv");
        let aux = VanillaCacheAux {
            loc_keys: vec![("english".into(), vec!["key_a".into(), "key_b".into()])],
            file_paths: vec!["gfx/interface/icon.dds".into()],
            var_names: vec!["my_var".into()],
            complex_enum_values: vec![("equipment_stat".into(), vec!["build_cost_ic".into()])],
            value_set_values: vec![("country_flag".into(), vec!["my_flag".into()])],
            scripted_loc_names: vec!["getsomevanillaloc".into()],
            scripted_gui_names: vec!["topbar_icon_click".into()],
        };
        assert_eq!(save(&idx, "hoi4", "v1.16.4", &path, aux).unwrap(), 2);

        let (game, fp, loaded) = load(&path).unwrap();
        assert_eq!(game, "hoi4");
        assert_eq!(fp, "v1.16.4");
        assert_eq!(loaded.per_type.get("spriteType").map(|v| v.len()), Some(2));
        // The per-instance source file survives the round trip (goto-into-vanilla).
        for (uri, _) in loaded.per_type.get("spriteType").unwrap() {
            assert_eq!(uri.as_ref(), "vanilla/x.gfx");
        }
        let sprite = loaded.per_type.get("spriteType").unwrap();
        assert!(Arc::ptr_eq(&sprite[0].0, &sprite[1].0));
        // Start AND end positions survive the round trip (v8 end plumbing).
        let by_name = |n: &str| {
            sprite
                .iter()
                .find(|(_, i)| i.name.as_str() == n)
                .map(|(_, i)| i.location)
                .unwrap()
        };
        let a = by_name("GFX_a");
        assert_eq!((a.line, a.col, a.end), (2, 1, (4, 1)));
        let b = by_name("GFX_b");
        assert_eq!((b.line, b.col, b.end), (5, 3, (9, 4)));
        // Explicit-field primary loc key survives the round trip (#141); an
        // instance with none stays None rather than picking up a stray value.
        let primary_loc_key = |n: &str| {
            sprite
                .iter()
                .find(|(_, i)| i.name.as_str() == n)
                .map(|(_, i)| i.primary_loc_key.clone())
                .unwrap()
        };
        assert_eq!(primary_loc_key("GFX_a"), Some("GFX_A_TITLE".to_string()));
        assert_eq!(primary_loc_key("GFX_b"), None);
        assert_eq!(loaded.aux.loc_keys.len(), 1);
        assert_eq!(loaded.aux.loc_keys[0].0, "english");
        assert_eq!(loaded.aux.file_paths, vec!["gfx/interface/icon.dds"]);
        assert_eq!(loaded.aux.var_names, vec!["my_var"]);
        assert_eq!(loaded.aux.complex_enum_values[0].0, "equipment_stat");
        assert_eq!(loaded.aux.value_set_values[0].0, "country_flag");
        assert_eq!(loaded.aux.scripted_loc_names, vec!["getsomevanillaloc"]);
        assert_eq!(loaded.aux.scripted_gui_names, vec!["topbar_icon_click"]);

        let mut idx2 = TypeIndex::new();
        idx2.merge_base_game_with_uris(loaded.per_type);
        assert!(idx2.contains("spriteType", "GFX_A"));
        assert!(idx2.contains("spriteType", "gfx_b"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn save_per_type_preserves_source_file() {
        // The LSP writes its vanilla cache via `save_per_type`; the per-instance
        // source path must survive save + load so a vanilla goto lands in the
        // real base-game file (the "<vanilla-cache>" sentinel bug, #62).
        let mut per: HashMap<String, Vec<(Arc<str>, TypeInstance)>> = HashMap::new();
        per.insert(
            "event".to_string(),
            vec![(
                Arc::from("/game/events/base.txt"),
                TypeInstance {
                    name: "base.1".into(),
                    location: SourceLocation {
                        line: 7,
                        col: 0,
                        end: (12, 1),
                    },
                    primary_loc_key: Some("base_1_title".into()),
                    required_loc_keys: Vec::new(),
                },
            )],
        );
        let path = std::env::temp_dir().join("cwtools_vanilla_cache_per_type_test.cwv");
        let aux = VanillaCacheAux::default();
        assert_eq!(save_per_type(&per, "hoi4", "vfp", &path, aux).unwrap(), 1);

        let (_, _, loaded) = load(&path).unwrap();
        let insts = loaded.per_type.get("event").expect("event instances");
        assert_eq!(insts.len(), 1);
        assert_eq!(
            insts[0].0.as_ref(),
            "/game/events/base.txt",
            "source file must round-trip, not fall back to empty"
        );
        assert_eq!(insts[0].1.location.line, 7);
        assert_eq!(insts[0].1.location.end, (12, 1));
        assert_eq!(
            insts[0].1.primary_loc_key.as_deref(),
            Some("base_1_title"),
            "primary loc key must round-trip through save_per_type too (#141)"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn old_json_cache_is_a_clean_miss() {
        let dir = std::env::temp_dir();
        let path = dir.join("cwtools_vanilla_cache_old_json.json");
        std::fs::write(&path, r#"{"version":3,"game":"hoi4","instances":[]}"#).unwrap();
        let err = load(&path).unwrap_err();
        assert!(err.to_string().contains("not a vanilla cache file"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn oversized_cache_is_refused_without_reading_it() {
        // The path comes from `--vanilla-cache` or an LSP client's
        // `vanillaCache` (#162), so an over-cap file has to read as a miss
        // rather than as an unbounded read. Sparse, so the test costs no disk.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(cache_file_name("hoi4", "huge"));
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(MAGIC).unwrap();
        file.write_all(&[CACHE_VERSION]).unwrap();
        file.set_len(MAX_CACHE_FILE_BYTES + 1).unwrap();
        drop(file);

        let err = load(&path).unwrap_err();
        assert!(err.to_string().contains("cache read cap"), "{err}");
    }

    #[test]
    fn cache_declaring_an_over_cap_body_is_refused() {
        // Proves `load` hands `decode_capped` its own cap rather than letting
        // anything through. zstd will not close a frame that lies about its
        // size, so the body is the prefix the encoder already emitted: enough
        // for the header check, and a truncated-frame error if the cap were not
        // applied.
        let mut encoder = zstd::stream::Encoder::new(Vec::new(), ZSTD_LEVEL).unwrap();
        encoder
            .set_pledged_src_size(Some(MAX_CACHE_DECODED_BYTES + 1))
            .unwrap();
        encoder.write_all(b"nowhere near that many bytes").unwrap();
        encoder.flush().unwrap();

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(cache_file_name("hoi4", "bomb"));
        let mut body = MAGIC.to_vec();
        body.push(CACHE_VERSION);
        body.extend_from_slice(encoder.get_ref());
        std::fs::write(&path, &body).unwrap();

        let err = load(&path).unwrap_err();
        assert!(err.to_string().contains("decompresses past"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn character_device_is_refused() {
        // `/dev/zero` reports length 0, so a size check alone waves it through
        // and then reads to EOF.
        let err = load(Path::new("/dev/zero")).unwrap_err();
        assert!(err.to_string().contains("not a regular file"), "{err}");
    }

    #[test]
    fn stale_version_cache_is_rejected() {
        // A cache written with the current framing but a prior version byte must
        // fail the version check (rebuilt), never be misread under the new layout.
        let idx = TypeIndex::new();
        let path = std::env::temp_dir().join("cwtools_vanilla_cache_stale_version.cwv");
        save(&idx, "hoi4", "vfp", &path, VanillaCacheAux::default()).unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[MAGIC.len()] = CACHE_VERSION - 1;
        std::fs::write(&path, &bytes).unwrap();
        let err = load(&path).unwrap_err();
        assert!(err.to_string().contains("unsupported"));
        let _ = std::fs::remove_file(&path);
    }

    // ── Corruption handling ──────────────────────────────────────────────────
    // Everything below has to come back as an `Err`, because that is what every
    // consumer relies on: `driver::load_fresh_vanilla_cache`, the LSP's scan and
    // config paths and the CLI all collapse an error to a miss and re-index the
    // install. A panic instead would take the server or CLI down over a file a
    // crash, a full disk or a stale format left behind. The `.cwb` parse cache
    // has the same suite (`cwtools_cache/tests/corrupt.rs`); this is the `.cwv`
    // half, which had none.

    /// A small but complete cache: two types, per-instance source files, and
    /// every aux list populated so the body is a real payload rather than a
    /// header and a stub.
    fn sample_cache_bytes() -> Vec<u8> {
        let mut per: HashMap<String, Vec<(Arc<str>, TypeInstance)>> = HashMap::new();
        per.insert(
            "spriteType".to_string(),
            vec![(
                Arc::from("gfx/interface/icons.gfx"),
                TypeInstance {
                    name: "GFX_a".into(),
                    location: SourceLocation {
                        line: 2,
                        col: 1,
                        end: (4, 1),
                    },
                    primary_loc_key: Some("GFX_A_TITLE".into()),
                    required_loc_keys: Vec::new(),
                },
            )],
        );
        per.insert(
            "event".to_string(),
            vec![(
                Arc::from("events/base.txt"),
                TypeInstance {
                    name: "base.1".into(),
                    location: SourceLocation {
                        line: 7,
                        col: 0,
                        end: (12, 1),
                    },
                    primary_loc_key: None,
                    required_loc_keys: Vec::new(),
                },
            )],
        );
        let aux = VanillaCacheAux {
            loc_keys: vec![("english".into(), vec!["key_a".into(), "key_b".into()])],
            file_paths: vec!["gfx/interface/icons.gfx".into(), "events/base.txt".into()],
            var_names: vec!["my_var".into()],
            complex_enum_values: vec![("equipment_stat".into(), vec!["build_cost_ic".into()])],
            value_set_values: vec![("country_flag".into(), vec!["my_flag".into()])],
            scripted_loc_names: vec!["getsomevanillaloc".into()],
            scripted_gui_names: vec!["topbar_icon_click".into()],
        };

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(cache_file_name("hoi4", "v1.16.4"));
        save_per_type(&per, "hoi4", "v1.16.4", &path, aux).unwrap();
        std::fs::read(&path).unwrap()
    }

    /// Write `bytes` to a temp `.cwv` and take them through the whole load path
    /// the consumers use: the read cap, the header, the version, zstd, rkyv and
    /// the restore into `VanillaCacheData`.
    fn load_bytes(bytes: &[u8]) -> std::io::Result<(String, String, VanillaCacheData)> {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(cache_file_name("hoi4", "corrupt"));
        std::fs::write(&path, bytes).unwrap();
        load(&path)
    }

    /// Load and reduce the restored cache to a comparable string, so a
    /// corruption that decodes into *different* data is distinguishable from one
    /// that reproduces the original. `per_type` is a `HashMap`, so it is sorted
    /// here rather than trusted to iterate in a fixed order.
    fn fingerprint_bytes(bytes: &[u8]) -> Option<String> {
        use std::fmt::Write;
        let (game, fp, data) = load_bytes(bytes).ok()?;
        let mut out = format!("{game}|{fp}");
        let mut types: Vec<_> = data.per_type.iter().collect();
        types.sort_by(|a, b| a.0.cmp(b.0));
        for (name, insts) in types {
            let _ = write!(out, "|T {name}");
            for (uri, inst) in insts {
                let _ = write!(
                    out,
                    "|I {uri} {} {:?} {:?}",
                    inst.name, inst.location, inst.primary_loc_key
                );
            }
        }
        let _ = write!(
            out,
            "|A {:?} {:?} {:?} {:?} {:?}",
            data.aux.loc_keys,
            data.aux.file_paths,
            data.aux.var_names,
            data.aux.complex_enum_values,
            data.aux.value_set_values
        );
        Some(out)
    }

    /// Sanity anchor: the unmodified bytes every test below mutates do load.
    #[test]
    fn sample_cache_bytes_load_clean() {
        let bytes = sample_cache_bytes();
        assert!(bytes.starts_with(MAGIC), "sample lost its magic");
        assert_eq!(bytes[MAGIC.len()], CACHE_VERSION, "sample version drifted");
        let (game, fp, data) = load_bytes(&bytes).expect("sample must load");
        assert_eq!((game.as_str(), fp.as_str()), ("hoi4", "v1.16.4"));
        assert_eq!(data.per_type.len(), 2);
    }

    /// A zero-byte file is what a crash mid-write leaves behind. It must not
    /// reach zstd or rkyv at all.
    #[test]
    fn zero_byte_cache_is_a_clean_miss() {
        let err = load_bytes(&[]).unwrap_err();
        assert!(
            err.to_string().contains("not a vanilla cache file"),
            "{err}"
        );
    }

    /// Any prefix shorter than magic+version is the same crash-mid-write case.
    #[test]
    fn cache_shorter_than_magic_plus_version_is_a_clean_miss() {
        let bytes = sample_cache_bytes();
        for len in 0..=MAGIC.len() {
            let err = load_bytes(&bytes[..len]).unwrap_err();
            assert!(
                err.to_string().contains("not a vanilla cache file"),
                "len {len} gave {err}"
            );
        }
    }

    /// A single wrong magic byte is enough, and so is a body with the header
    /// sliced off (what a pre-v4 raw file would look like).
    #[test]
    fn a_corrupted_magic_is_a_clean_miss() {
        let bytes = sample_cache_bytes();
        for i in 0..MAGIC.len() {
            let mut corrupt = bytes.clone();
            corrupt[i] ^= 0xff;
            let err = load_bytes(&corrupt).unwrap_err();
            assert!(
                err.to_string().contains("not a vanilla cache file"),
                "magic byte {i} gave {err}"
            );
        }

        let err = load_bytes(&bytes[MAGIC.len() + 1..]).unwrap_err();
        assert!(
            err.to_string().contains("not a vanilla cache file"),
            "{err}"
        );
    }

    /// The point of `CACHE_VERSION`: a cache from any other layout is refused
    /// rather than reinterpreted under the current one. Eleven format bumps
    /// have happened and only the immediately-previous one was covered.
    #[test]
    fn every_other_version_byte_is_a_clean_miss() {
        let bytes = sample_cache_bytes();
        for version in [0u8, 1, 4, CACHE_VERSION - 1, CACHE_VERSION + 1, 255] {
            let mut corrupt = bytes.clone();
            corrupt[MAGIC.len()] = version;
            let err = load_bytes(&corrupt).unwrap_err();
            assert!(
                err.to_string().contains("unsupported"),
                "version {version} gave {err}"
            );
        }
    }

    /// A valid header over a body that stops early: the write was interrupted
    /// after the header landed, or the disk filled up. zstd and rkyv have to
    /// reject it — a partial cache served as a hit is a base-game index missing
    /// whatever the truncation cut off.
    #[test]
    fn a_truncated_body_is_a_clean_miss() {
        let bytes = sample_cache_bytes();
        let body_start = MAGIC.len() + 1;
        assert!(
            bytes.len() > body_start + 8,
            "sample body too small to truncate"
        );
        for len in body_start..bytes.len() {
            assert!(
                load_bytes(&bytes[..len]).is_err(),
                "truncating to {len} of {} bytes loaded as valid",
                bytes.len()
            );
        }
    }

    /// Bit rot in the compressed body. Every single-bit flip must either fail to
    /// load or restore the cache exactly. Silently decoding into *different* data
    /// is the bad case: rkyv validates structure, not content, so a flipped
    /// instance name or line number sails through and the editor resolves
    /// base-game references against an index that never existed.
    ///
    /// The zstd frame checksum is what closes that. With it off, 191 of 846 flips
    /// here restored an altered cache and were served as hits (#245).
    #[test]
    fn a_bit_flipped_body_never_yields_altered_data() {
        let bytes = sample_cache_bytes();
        let body_start = MAGIC.len() + 1;
        let clean = fingerprint_bytes(&bytes).expect("sample must load");
        let mut same = 0usize;
        let mut altered = 0usize;

        for i in body_start..bytes.len() {
            for bit in [0u8, 3, 7] {
                let mut corrupt = bytes.clone();
                corrupt[i] ^= 1 << bit;
                // Reaching the next line at all is the test: a panic inside the
                // load path fails here instead of degrading to a cache miss.
                if let Some(got) = fingerprint_bytes(&corrupt) {
                    if got == clean {
                        same += 1;
                    } else {
                        altered += 1;
                    }
                }
            }
        }

        let attempts = (bytes.len() - body_start) * 3;
        assert_eq!(
            altered, 0,
            "{altered} of {attempts} bit flips restored different cache data and \
             were accepted as a cache hit"
        );
        // A handful land in frame bits the decoder ignores and reproduce the
        // original exactly. Harmless, but if it were most of them the checksum
        // would not be doing anything.
        assert!(
            same * 4 < attempts,
            "{same} of {attempts} bit flips round-tripped unchanged"
        );
    }

    /// The frame checksum is a write-side setting only. A `.cwv` written before
    /// it was turned on carries no checksum and must still load, which is why
    /// enabling it needed no `CACHE_VERSION` bump and no cache wipe.
    #[test]
    fn a_checksumless_frame_still_loads() {
        let bytes = sample_cache_bytes();
        let body_start = MAGIC.len() + 1;
        let mut raw = Vec::new();
        decode_capped(&bytes[body_start..], MAX_CACHE_DECODED_BYTES, &mut raw).unwrap();

        // `zstd::encode_all` is what the writer used before, and it writes no
        // frame checksum.
        let mut old_style = MAGIC.to_vec();
        old_style.push(CACHE_VERSION);
        old_style.extend_from_slice(&zstd::encode_all(&raw[..], ZSTD_LEVEL).unwrap());

        assert_eq!(
            fingerprint_bytes(&old_style).expect("a checksumless frame must still load"),
            fingerprint_bytes(&bytes).unwrap(),
            "old and new frames must restore the same cache"
        );
    }

    /// A body that is not zstd at all: something else ended up in the cache dir
    /// under a `.cwv` name and happened to carry the header.
    #[test]
    fn a_non_zstd_body_is_a_clean_miss() {
        let mut bytes = MAGIC.to_vec();
        bytes.push(CACHE_VERSION);
        bytes.extend_from_slice(b"this is not a zstd frame, it is prose");
        assert!(load_bytes(&bytes).is_err());
    }

    /// A missing cache file is the ordinary cold-start miss, and has to surface
    /// as an error the caller already handles rather than anything special.
    #[test]
    fn a_missing_cache_file_is_a_clean_miss() {
        let tmp = tempfile::tempdir().unwrap();
        let err = load(&tmp.path().join(cache_file_name("hoi4", "absent"))).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound, "{err}");
    }

    #[test]
    fn ruleset_shape_hash_is_stable_and_sensitive() {
        use cwtools_rules::rules_types::{PathOptions, TypeDefinition};

        let mk = |name: &str, name_field: Option<&str>| TypeDefinition {
            name: name.to_string(),
            name_field: name_field.map(str::to_string),
            path_options: PathOptions {
                paths: vec!["common/foo".into()],
                path_strict: false,
                path_file: None,
                path_extension: None,
                paths_lower: vec![],
                ..Default::default()
            },
            subtypes: vec![],
            type_key_filter: None,
            skip_root_key: vec![],
            starts_with: None,
            type_per_file: false,
            key_prefix: None,
            warning_only: false,
            unique: false,
            should_be_referenced: false,
            localisation: vec![],
            graph_related_types: vec![],
            modifiers: vec![],
        };

        let mut a = RuleSet::new();
        a.types = vec![mk("event", None), mk("tech", Some("id"))];
        let mut b = RuleSet::new();
        // Same content, different declaration order → same hash (order-independent).
        b.types = vec![mk("tech", Some("id")), mk("event", None)];
        assert_eq!(ruleset_shape_hash(&a), ruleset_shape_hash(&b));

        // A meaningful shape change (name_field) flips the hash.
        let mut c = RuleSet::new();
        c.types = vec![mk("event", Some("id")), mk("tech", Some("id"))];
        assert_ne!(ruleset_shape_hash(&a), ruleset_shape_hash(&c));
    }
}
