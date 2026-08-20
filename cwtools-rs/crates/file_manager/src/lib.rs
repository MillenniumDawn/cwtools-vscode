pub mod file_manager;

pub use file_manager::{
    DirectoryType, FileEncoding, FileError, FileKind, FileManager, FileManagerConfig,
    ModDescriptor, ParsedFile, ResolvedMod, SCRIPT_EXTENSIONS, ScanBudget, ScanBytes,
    classify_directory, discover_files_multi_mod, expand_multiple_mods, is_excluded_dir,
    is_excluded_root_dir, is_loc_ext, read_text, read_text_capped, read_text_capped_with_encoding,
    read_text_with_encoding,
};
