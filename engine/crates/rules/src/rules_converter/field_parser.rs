use super::*;

pub(crate) fn field_from_string(s: &str) -> NewField {
    let trimmed = s.trim().trim_matches('"');

    if let Some(field) = parse_simple_keyword(trimmed) {
        return field;
    }
    if let Some(field) = parse_filepath_bracket(trimmed) {
        return field;
    }
    if let Some(field) = parse_numeric_range(trimmed) {
        return field;
    }
    if let Some(field) = parse_named_bracket(trimmed) {
        return field;
    }
    if let Some(field) = parse_scope_alias_bracket(trimmed) {
        return field;
    }
    if let Some(field) = parse_variable_value_range(trimmed) {
        return field;
    }
    if let Some(field) = parse_misc_bracket(trimmed) {
        return field;
    }
    if let Some(field) = parse_type_form(trimmed) {
        return field;
    }

    NewField::SpecificField(trimmed.to_string())
}

fn parse_simple_keyword(trimmed: &str) -> Option<NewField> {
    Some(match trimmed {
        "scalar" => NewField::ScalarField,
        "math_expr" => NewField::ValueField(ValueType::MathExpr),
        "bool" => NewField::ValueField(ValueType::Bool),
        "int" => NewField::ValueField(ValueType::Int {
            min: INT_MIN,
            max: INT_MAX,
        }),
        "float" => NewField::ValueField(ValueType::Float {
            min: FLOAT_MIN,
            max: FLOAT_MAX,
        }),
        "percentage_field" => NewField::ValueField(ValueType::Percent),
        "localisation" => NewField::LocalisationField {
            synced: false,
            is_inline: false,
        },
        "localisation_synced" => NewField::LocalisationField {
            synced: true,
            is_inline: false,
        },
        "localisation_inline" => NewField::LocalisationField {
            synced: false,
            is_inline: true,
        },
        "filepath" => NewField::FilepathField {
            prefix: None,
            extension: None,
        },
        "date_field" => NewField::ValueField(ValueType::Date),
        "datetime_field" => NewField::ValueField(ValueType::DateTime),
        "scope_field" => NewField::ScopeField(vec!["any".to_string()]),
        "variable_field" => NewField::VariableField {
            is_int: false,
            is_32bit: false,
            min: FLOAT_MIN,
            max: FLOAT_MAX,
        },
        "int_variable_field" => NewField::VariableField {
            is_int: true,
            is_32bit: false,
            min: INT_MIN as f64,
            max: INT_MAX as f64,
        },
        "variable_field_32" => NewField::VariableField {
            is_int: false,
            is_32bit: true,
            min: FLOAT_MIN,
            max: FLOAT_MAX,
        },
        "int_variable_field_32" => NewField::VariableField {
            is_int: true,
            is_32bit: true,
            min: INT_MIN as f64,
            max: INT_MAX as f64,
        },
        "value_field" => NewField::ValueScopeMarkerField {
            is_int: false,
            min: FLOAT_MIN,
            max: FLOAT_MAX,
        },
        "int_value_field" => NewField::ValueScopeMarkerField {
            is_int: true,
            min: INT_MIN as f64,
            max: INT_MAX as f64,
        },
        "portrait_dna_field" => NewField::ValueField(ValueType::Ck2Dna),
        "portrait_properties_field" => NewField::ValueField(ValueType::Ck2DnaProperty),
        "ck2_dna_field" => NewField::ValueField(ValueType::Ck2Dna),
        "ck2_dna_property_field" => NewField::ValueField(ValueType::Ck2DnaProperty),
        "ir_family_name_field" => NewField::ValueField(ValueType::IrFamilyName),
        "ir_country_tag_field" => NewField::MarkerField(Marker::IrCountryTag),
        "colour_field" | "color_field" => NewField::MarkerField(Marker::ColourField),
        "ignore_field" => NewField::IgnoreMarkerField,
        "stellaris_name_format" => NewField::ValueField(ValueType::StlNameFormat(String::new())),
        "percent_field" => NewField::ValueField(ValueType::Percent),
        _ => return None,
    })
}

fn parse_filepath_bracket(trimmed: &str) -> Option<NewField> {
    let inner = strip_bracket(trimmed, "filepath")?;
    Some(if inner.contains(',') {
        let mut parts = inner.splitn(2, ',');
        let folder = parts.next().unwrap_or("").to_string();
        let ext = parts.next().unwrap_or("").to_string();
        NewField::FilepathField {
            prefix: Some(folder),
            extension: Some(ext),
        }
    } else {
        NewField::FilepathField {
            prefix: Some(inner.to_string()),
            extension: None,
        }
    })
}

fn parse_numeric_range(trimmed: &str) -> Option<NewField> {
    if let Some(inner) = strip_bracket(trimmed, "int") {
        if let Some((min_s, max_s)) = inner.split_once("..") {
            let min = parse_int_sentinel(min_s.trim());
            let max = parse_int_sentinel(max_s.trim());
            if let (Some(mn), Some(mx)) = (min, max) {
                return Some(NewField::ValueField(ValueType::Int { min: mn, max: mx }));
            }
        }
        return Some(NewField::ValueField(ValueType::Int {
            min: INT_MIN,
            max: INT_MAX,
        }));
    }

    if let Some(inner) = strip_bracket(trimmed, "float") {
        if let Some((min_s, max_s)) = inner.split_once("..") {
            let min = parse_float_sentinel(min_s.trim());
            let max = parse_float_sentinel(max_s.trim());
            if let (Some(mn), Some(mx)) = (min, max) {
                return Some(NewField::ValueField(ValueType::Float { min: mn, max: mx }));
            }
        }
        return Some(NewField::ValueField(ValueType::Float {
            min: FLOAT_MIN,
            max: FLOAT_MAX,
        }));
    }

    None
}

fn parse_named_bracket(trimmed: &str) -> Option<NewField> {
    if let Some(name) = strip_bracket(trimmed, "enum") {
        return Some(NewField::ValueField(ValueType::Enum(name.to_string())));
    }
    if let Some(name) = strip_bracket(trimmed, "complex_enum") {
        return Some(NewField::ValueField(ValueType::Enum(name.to_string())));
    }
    if let Some(var) = strip_bracket(trimmed, "value") {
        return Some(NewField::VariableGetField(var.to_string()));
    }
    if let Some(var) = strip_bracket(trimmed, "value_set") {
        return Some(NewField::VariableSetField(var.to_string()));
    }
    None
}

fn parse_scope_alias_bracket(trimmed: &str) -> Option<NewField> {
    if let Some(scope) = strip_bracket(trimmed, "scope") {
        return Some(NewField::ScopeField(vec![scope.to_string()]));
    }
    if let Some(scope) = strip_bracket(trimmed, "event_target") {
        return Some(NewField::ScopeField(vec![scope.to_string()]));
    }
    if strip_bracket(trimmed, "scope_group").is_some() {
        return Some(NewField::ScopeField(vec!["any".to_string()]));
    }
    if let Some(alias) = strip_bracket(trimmed, "alias_keys_field") {
        return Some(NewField::AliasValueKeysField(alias.to_string()));
    }
    if let Some(alias) =
        strip_bracket(trimmed, "alias_name").or_else(|| strip_bracket(trimmed, "alias_match_left"))
    {
        return Some(NewField::AliasField(alias.to_string()));
    }
    if let Some(alias) = strip_bracket(trimmed, "single_alias_right") {
        return Some(NewField::SingleAliasField(alias.to_string()));
    }
    None
}

fn parse_variable_value_range(trimmed: &str) -> Option<NewField> {
    if let Some(inner) = strip_bracket(trimmed, "variable_field") {
        let (mn, mx) = parse_float_range(inner, FLOAT_MIN, FLOAT_MAX);
        return Some(NewField::VariableField {
            is_int: false,
            is_32bit: false,
            min: mn,
            max: mx,
        });
    }
    if let Some(inner) = strip_bracket(trimmed, "int_variable_field") {
        let (mn, mx) = parse_int_range_as_float(inner, INT_MIN as f64, INT_MAX as f64);
        return Some(NewField::VariableField {
            is_int: true,
            is_32bit: false,
            min: mn,
            max: mx,
        });
    }
    if let Some(inner) = strip_bracket(trimmed, "variable_field_32") {
        let (mn, mx) = parse_float_range(inner, FLOAT_MIN, FLOAT_MAX);
        return Some(NewField::VariableField {
            is_int: false,
            is_32bit: true,
            min: mn,
            max: mx,
        });
    }
    if let Some(inner) = strip_bracket(trimmed, "int_variable_field_32") {
        let (mn, mx) = parse_int_range_as_float(inner, INT_MIN as f64, INT_MAX as f64);
        return Some(NewField::VariableField {
            is_int: true,
            is_32bit: true,
            min: mn,
            max: mx,
        });
    }
    if let Some(inner) = strip_bracket(trimmed, "value_field") {
        let (mn, mx) = parse_float_range(inner, FLOAT_MIN, FLOAT_MAX);
        return Some(NewField::ValueScopeMarkerField {
            is_int: false,
            min: mn,
            max: mx,
        });
    }
    if let Some(inner) = strip_bracket(trimmed, "int_value_field") {
        let (mn, mx) = parse_int_range_as_float(inner, INT_MIN as f64, INT_MAX as f64);
        return Some(NewField::ValueScopeMarkerField {
            is_int: true,
            min: mn,
            max: mx,
        });
    }
    None
}

fn parse_misc_bracket(trimmed: &str) -> Option<NewField> {
    if let Some(folder) = strip_bracket(trimmed, "icon") {
        return Some(NewField::IconField(folder.to_string()));
    }
    if let Some(var) = strip_bracket(trimmed, "stellaris_name_format") {
        return Some(NewField::ValueField(ValueType::StlNameFormat(
            var.to_string(),
        )));
    }
    if strip_bracket(trimmed, "colour").is_some() {
        return Some(NewField::MarkerField(Marker::ColourField));
    }
    None
}

fn parse_type_form(trimmed: &str) -> Option<NewField> {
    if let Some(type_ref) = trimmed
        .strip_prefix('<')
        .and_then(|t| t.strip_suffix('>'))
        .filter(|t| !t.contains(' '))
    {
        return Some(NewField::TypeField(TypeType::Simple(type_ref.to_string())));
    }

    if trimmed.contains('<') && trimmed.contains('>') {
        let s = trimmed.trim_matches('"');
        if let (Some(pi), Some(si)) = (s.find('<'), s.find('>'))
            && pi < si
        {
            let prefix = s[..pi].to_string();
            let name = s[pi + 1..si].to_string();
            let suffix = s[si + 1..].to_string();
            return Some(NewField::TypeField(TypeType::Complex {
                prefix,
                name,
                suffix,
            }));
        }
    }
    None
}

fn strip_bracket<'a>(trimmed: &'a str, prefix: &str) -> Option<&'a str> {
    trimmed
        .strip_prefix(prefix)?
        .strip_prefix('[')?
        .strip_suffix(']')
}

fn parse_int_sentinel(s: &str) -> Option<i32> {
    match s {
        "inf" => Some(INT_MAX),
        "-inf" => Some(INT_MIN),
        other => other.parse::<i32>().ok(),
    }
}

fn parse_float_sentinel(s: &str) -> Option<f64> {
    match s {
        "inf" => Some(FLOAT_MAX),
        "-inf" => Some(FLOAT_MIN),
        other => other.parse::<f64>().ok(),
    }
}

fn parse_float_range(inner: &str, default_min: f64, default_max: f64) -> (f64, f64) {
    if let Some((min_s, max_s)) = inner.split_once("..") {
        let mn = parse_float_sentinel(min_s.trim()).unwrap_or(default_min);
        let mx = parse_float_sentinel(max_s.trim()).unwrap_or(default_max);
        (mn, mx)
    } else {
        (default_min, default_max)
    }
}

fn parse_int_range_as_float(inner: &str, default_min: f64, default_max: f64) -> (f64, f64) {
    if let Some((min_s, max_s)) = inner.split_once("..") {
        let mn = parse_int_sentinel(min_s.trim())
            .map(|v| v as f64)
            .unwrap_or(default_min);
        let mx = parse_int_sentinel(max_s.trim())
            .map(|v| v as f64)
            .unwrap_or(default_max);
        (mn, mx)
    } else {
        (default_min, default_max)
    }
}
