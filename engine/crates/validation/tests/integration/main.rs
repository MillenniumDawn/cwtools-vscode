//! Integration tests for the validation crate, linked as one binary so they
//! share `common`. `scope_probe_allocations` stays out because it installs a
//! global allocator.

mod common;

mod alias_recursion_budget;
mod alias_value_set_case;
mod basics;
mod cardinality_keys;
mod cardinality_position;
mod default_bool;
mod derived_equipment;
mod dispatch_resolution;
mod duplicate_type_defs;
mod enum_scope_keys;
mod error_if_only_match;
mod filepath_texture;
mod gfx_gui_sprites;
mod gui_music_enum_false_positives;
mod inline_script_expansion;
mod leafvalue_cardinality;
mod localisation_field;
mod mathexpr_codes;
mod objecttypes_disambiguation;
mod position_resolver;
mod rules_engine_codes;
mod seg_codes;
mod stellaris_config;
mod structural_codes;
mod subtype_completion_union;
mod subtype_membership;
mod type_routing_and_inline;
mod type_warning_only;
mod var_codes;
