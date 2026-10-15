pub const COMPLETION_RULES: &str = r#"
types = {
    type[focus] = {
        path = "game/common/national_focus"
    }
    type[decision] = {
        path = "game/common/decisions"
    }
    type[mio] = {
        path = "game/common/military_industrial_organization/organizations"
    }
    type[event] = {
        path = "game/events"
    }
}
decision = {
    allowed = {
        alias_name[trigger] = alias_match_left[trigger]
    }
    cost = int
    set_math = {
        value_set[variable] = math_expr
        value_set[variable] = scalar
    }
}
focus = {
    id = scalar
    x = int
    y = int
    cost = float
    completion_reward = {
        alias_name[effect] = alias_match_left[effect]
    }
    available = {
        alias_name[trigger] = alias_match_left[trigger]
    }
}
event = {
    id = scalar
    title = scalar
    trigger = {
        alias_name[trigger] = alias_match_left[trigger]
    }
    immediate = {
        alias_name[effect] = alias_match_left[effect]
    }
    option = {
        name = scalar
    }
}
alias[mathexpr:add] = math_expr
alias[mathexpr:subtract] = math_expr
alias[mathexpr:multiply] = math_expr
mio = {
    name = scalar
    equipment_bonus = {
        <equipment> = {
            alias_name[modifier] = alias_match_left[modifier]
        }
    }
}
alias[trigger:has_completed_focus] = <focus>
### Always evaluates to true.
alias[trigger:always] = bool
alias[effect:add_political_power] = int
modifiers = {
    build_cost_ic = economy
    production_speed_factor = economy
}
"#;

/// Rules mirroring the REAL HOI4 config shapes: MIO trait equipment_bonus is
/// keyed by enum[equipment_stat] (a complex enum collected from
/// common/script_enums.txt), has_idea reads enum[idea_name], and
/// has_country_flag reads value[country_flag] (members written by
/// set_country_flag).
pub const DYNAMIC_RULES: &str = r#"
types = {
    type[mio] = {
        path = "game/common/military_industrial_organization/organizations"
    }
    type[decision] = {
        path = "game/common/decisions"
    }
}
enums = {
    complex_enum[equipment_stat] = {
        path = "game/common"
        path_file = "script_enums.txt"
        start_from_root = yes
        name = {
            script_enum_equipment_stat = {
                enum_name
            }
        }
    }
    complex_enum[idea_name] = {
        path = "game/common/ideas"
        name = {
            scalar = {
                enum_name = {
                }
            }
        }
    }
}
mio = {
    name = scalar
    trait = {
        token = scalar
        equipment_bonus = {
            ## cardinality = ~1..inf
            enum[equipment_stat] = variable_field
            ## cardinality = 0..1
            instant = bool
        }
    }
}
decision = {
    allowed = {
        alias_name[trigger] = alias_match_left[trigger]
    }
    complete_effect = {
        alias_name[effect] = alias_match_left[effect]
    }
    cost = int
}
### Does the country have this idea
alias[trigger:has_idea] = enum[idea_name]
### Has the country flag been set
alias[trigger:has_country_flag] = value[country_flag]
alias[effect:set_country_flag] = value_set[country_flag]
"#;

/// Rules for the editor-capability round-trips below: an entity with a colour
/// block in both conventions, a typed reference, an enum, a localisation field
/// and a trigger alias.
pub const EDITOR_RULES: &str = r#"
types = {
    type[focus] = {
        path = "game/common/national_focus"
    }
    type[ideology] = {
        path = "game/common/ideologies"
    }
}
focus = {
    id = scalar
    cost = int
    text = localisation
    mood = enum[mood]
    prerequisite = <focus>
    available = {
        alias_name[trigger] = alias_match_left[trigger]
    }
}
ideology = {
    color = {
        ## cardinality = 3..3
        int
    }
    color = {
        ## cardinality = 3..3
        float
    }
    dynamic_faction_names = {
        ## cardinality = 3..3
        int
    }
}
alias[trigger:has_completed_focus] = <focus>
alias[trigger:always] = bool
enums = {
    enum[mood] = {
        calm
        angry
    }
}
"#;

/// Rules exercising every navigable reference kind goto must resolve.
pub const GOTO_RULES: &str = r#"
types = {
    type[focus] = { path = "game/common/national_focus" }
    type[oob] = { path = "game/history/units" }
    type[character] = { path = "game/common/characters" }
    type[special_project] = { path = "game/common/special_projects" }
    type[scripted_effect] = { path = "game/common/scripted_effects" }
    type[decision] = { path = "game/common/decisions" }
    type[on_action] = { path = "game/common/on_actions" }
    ## type_key_filter = on_weekly
    type[on_weekly] = {
        path = "game/common/on_actions"
        skip_root_key = on_actions
    }
}
links = {
    sp = {
        prefix = sp:
        output_scope = special_project
        input_scopes = country
        from_data = yes
        data_source = <special_project>
    }
    character = {
        output_scope = character
        input_scopes = country
        from_data = yes
        data_source = <character>
    }
}
decision = {
    ## cardinality = 0..1
    has_focus = <focus>
    ## cardinality = 0..1
    load_oob = <oob>
    ## cardinality = 0..1
    localization_key = localisation
    ## cardinality = 0..1
    complete_special_project = scope[special_project]
    ## cardinality = 0..1
    terrain = enum[terrain]
    ## cardinality = 0..1
    available = {
        alias_name[trigger] = alias_match_left[trigger]
    }
    ## cardinality = 0..1
    complete_effect = {
        alias_name[effect] = alias_match_left[effect]
    }
    ## cardinality = 0..inf
    <character> = {
        is_enabled = bool
    }
}
alias[trigger:always] = bool
alias[effect:<scripted_effect>] = yes
focus = { x = bool }
oob = { y = bool }
character = { name = scalar }
special_project = { z = bool }
scripted_effect = { alias_name[effect] = alias_match_left[effect] }
on_action = {
    ## cardinality = 0..inf
    on_weekly = single_alias_right[country_event_effect]
}
single_alias[country_event_effect] = {
    ## cardinality = 0..inf
    effect = {
        alias_name[effect] = alias_match_left[effect]
    }
}
enums = {
    enum[terrain] = {
        plains
        forest
    }
}
"#;

/// A game file that validates under GOTO_RULES (references an undefined focus,
/// so it produces one diagnostic). The storm tests count `[validate]` /
/// publishDiagnostics frames, so its exact content only matters where a test
/// checks for a non-empty diagnostic (the open-then-close race test).
pub const STORM_FILE: &str = "my_dec = {\n    has_focus = my_focus\n}\n";

/// `text` with the UTF-8 byte order mark the game's localisation files start with.
pub fn bom(text: &str) -> Vec<u8> {
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(text.as_bytes());
    bytes
}

/// A localisation file's text as the game ships it: byte order mark, header,
/// then `body`. Written to disk as is, it is also the text an editor opens.
pub fn loc(body: &str) -> String {
    format!("\u{FEFF}l_english:\n{body}")
}

/// A `decision` type under `game/common/decisions`, enough for the diagnostics
/// that need no rule of their own (CW281 and friends).
pub const DECISION_RULES: &str = r#"
types = {
    type[decision] = { path = "game/common/decisions" }
}
"#;
