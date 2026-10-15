//! The completion-only subtype union (cwtools-vscode#89) must NOT leak into
//! validation: the validator still resolves the exact matching subtype set, so a
//! field that belongs only to an inactive subtype is still flagged unexpected.

use cwtools_validation::Game;

use super::common::{Opts, validate};

const CWT: &str = r#"
types = {
    type[thing] = {
        path = "game/common/things"
        subtype[special] = {
            is_special = yes
        }
    }
}
thing = {
    ## cardinality = 0..1
    is_special = bool
    ## cardinality = 0..1
    common_field = scalar
    subtype[special] = {
        ## cardinality = 0..1
        special_only = scalar
    }
}
"#;

const THING_FILE: Opts<'static> = Opts {
    path: "game/common/things/test.txt",
    game: Some(Game::Hoi4),
    ..Opts::DEFAULT
};

#[test]
fn inactive_subtype_field_is_still_unexpected_in_validation() {
    // No `is_special`, so subtype `special` is inactive: `special_only` must be
    // flagged. If the completion union leaked into validation, it would be
    // silently accepted.
    let errors = validate(
        CWT,
        "my_thing = {\n    common_field = ok\n    special_only = leaked\n}\n",
        &THING_FILE,
    );
    assert!(
        errors.iter().any(|e| e.message.contains("special_only")),
        "inactive subtype field must be flagged unexpected, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn active_subtype_field_is_accepted_in_validation() {
    // With `is_special = yes` the subtype activates and `special_only` is valid —
    // confirms the field is real and only the gating differs.
    let errors = validate(
        CWT,
        "my_thing = {\n    is_special = yes\n    special_only = fine\n}\n",
        &THING_FILE,
    );
    assert!(
        !errors.iter().any(|e| e.message.contains("special_only")),
        "active subtype field must be accepted, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}
