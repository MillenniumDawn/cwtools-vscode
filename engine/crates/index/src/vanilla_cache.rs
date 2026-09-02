// zstd level, the atomic write and the bounded read/decode used by the `.cwb`
use cwtools_cache::io::{ZSTD_LEVEL, decode_capped, read_capped, write_atomically};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;

use cwtools_rules::rules_types::{RuleSet, SkipRootKey};

use crate::{SourceLocation, TypeIndex, TypeInstance};

const MAGIC: &[u8; 4] = b"CWV\x00";

const FILE_PREFIX: &str = "vanilla-";
const FILE_EXT: &str = ".cwv";

// v2 adds `fingerprint` (game version) so a cache can be validated against the
// v3 folds the ruleset shape into the fingerprint (see `combined_fingerprint`):
// v4 switches the on-disk format from JSON to magic+version-framed zstd(rkyv)
// live-scanned instances. v7 files lack the end fields, so the rkyv layout
// lowercased), so a case-sensitive run (--case-sensitive-files) can enforce
// lowercased, must rebuild or a case-sensitive run would flag every vanilla
// lack it and restore it as `None` (#141).
// on every use (#348).
// so `[!name]` calls can resolve callbacks supplied by vanilla (#350).
const CACHE_VERSION: u8 = 13;

/// Hard caps on a `.cwv` read and on what its body may decompress to. The path
/// comes from `--vanilla-cache` or an LSP client's `vanillaCache` (#162), so a
/// few kilobytes of `CWV`-prefixed zstd must not be able to expand until the
const MAX_CACHE_FILE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_CACHE_DECODED_BYTES: u64 = 512 * 1024 * 1024;

#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
struct CachedInstance {
    t: String,
    n: String,
    f: String,
    l: u32,
    c: u16,
    el: u32,
    ec: u16,
    p: Option<String>,
}

#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
struct VanillaCacheFile {
    game: String,
    /// Game-version fingerprint (see [`fingerprint`]). A cache is valid only for
    /// the install that produced this fingerprint.
    fingerprint: String,
    instances: Vec<CachedInstance>,
    loc_keys: Vec<(String, Vec<String>)>,
    file_paths: Vec<String>,
    var_names: Vec<String>,
    complex_enum_values: Vec<(String, Vec<String>)>,
    value_set_values: Vec<(String, Vec<String>)>,
    scripted_loc_names: Vec<String>,
    scripted_gui_names: Vec<String>,
}

/// editor cannot quietly restore a subset of what the CLI wrote (#283).
#[derive(Clone, Debug, Default)]
pub struct VanillaCacheAux {
    pub loc_keys: Vec<(String, Vec<String>)>,
    pub file_paths: Vec<String>,
    pub var_names: Vec<String>,
    pub complex_enum_values: Vec<(String, Vec<String>)>,
    pub value_set_values: Vec<(String, Vec<String>)>,
    pub scripted_loc_names: Vec<String>,
    pub scripted_gui_names: Vec<String>,
}

#[derive(Debug)]
pub struct VanillaCacheData {
    pub per_type: HashMap<String, Vec<(Arc<str>, TypeInstance)>>,
    pub aux: VanillaCacheAux,
}

/// A stable fingerprint of a base-game install, used to invalidate the cache
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
    let h = fnv1a(dir.to_string_lossy().as_bytes(), 0xcbf2_9ce4_8422_2325u64);
    format!("unknown-{h:016x}")
}

/// FNV-1a over `bytes`, continuing from `hash`. A stable, dependency-free hash
/// versions) so a cache fingerprint stays comparable across restarts/toolchains.
fn fnv1a(bytes: &[u8], mut hash: u64) -> u64 {
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

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
pub fn combined_fingerprint(dir: &Path, ruleset: &RuleSet) -> String {
    format!("{}|rs:{}", fingerprint(dir), ruleset_shape_hash(ruleset))
}

/// Filename of the cache for `game` at `fingerprint`, versioned in the name so
/// a fingerprint carries the game version verbatim, and a separator in it would
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

/// [`cache_file_name`] shape, a regular file rather than a symlink or a
/// directory an LSP client chose (#159), so it has to recognise a cache by what
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
    // served as a cache hit. The checksum turns that into a decode error, which
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
    // CLI share a cache dir and key the file by game + fingerprint) destroyed
    write_atomically(path, |file| {
        file.write_all(MAGIC)?;
        file.write_all(&[CACHE_VERSION])?;
        file.write_all(&compressed)
    })?;
    Ok(count)
}

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
        for (uri, _) in loaded.per_type.get("spriteType").unwrap() {
            assert_eq!(uri.as_ref(), "vanilla/x.gfx");
        }
        let sprite = loaded.per_type.get("spriteType").unwrap();
        assert!(Arc::ptr_eq(&sprite[0].0, &sprite[1].0));
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
        // `vanillaCache` (#162), so an over-cap file has to read as a miss
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
        // for the header check, and a truncated-frame error if the cap were not
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
        let err = load(Path::new("/dev/zero")).unwrap_err();
        assert!(err.to_string().contains("not a regular file"), "{err}");
    }

    #[test]
    fn stale_version_cache_is_rejected() {
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

    /// the consumers use: the read cap, the header, the version, zstd, rkyv and
    fn load_bytes(bytes: &[u8]) -> std::io::Result<(String, String, VanillaCacheData)> {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(cache_file_name("hoi4", "corrupt"));
        std::fs::write(&path, bytes).unwrap();
        load(&path)
    }

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

    #[test]
    fn sample_cache_bytes_load_clean() {
        let bytes = sample_cache_bytes();
        assert!(bytes.starts_with(MAGIC), "sample lost its magic");
        assert_eq!(bytes[MAGIC.len()], CACHE_VERSION, "sample version drifted");
        let (game, fp, data) = load_bytes(&bytes).expect("sample must load");
        assert_eq!((game.as_str(), fp.as_str()), ("hoi4", "v1.16.4"));
        assert_eq!(data.per_type.len(), 2);
    }

    /// reach zstd or rkyv at all.
    #[test]
    fn zero_byte_cache_is_a_clean_miss() {
        let err = load_bytes(&[]).unwrap_err();
        assert!(
            err.to_string().contains("not a vanilla cache file"),
            "{err}"
        );
    }

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

    /// after the header landed, or the disk filled up. zstd and rkyv have to
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

    /// is the bad case: rkyv validates structure, not content, so a flipped
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
        // original exactly. Harmless, but if it were most of them the checksum
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
    #[test]
    fn a_non_zstd_body_is_a_clean_miss() {
        let mut bytes = MAGIC.to_vec();
        bytes.push(CACHE_VERSION);
        bytes.extend_from_slice(b"this is not a zstd frame, it is prose");
        assert!(load_bytes(&bytes).is_err());
    }

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
        b.types = vec![mk("tech", Some("id")), mk("event", None)];
        assert_eq!(ruleset_shape_hash(&a), ruleset_shape_hash(&b));

        let mut c = RuleSet::new();
        c.types = vec![mk("event", Some("id")), mk("tech", Some("id"))];
        assert_ne!(ruleset_shape_hash(&a), ruleset_shape_hash(&c));
    }
}
