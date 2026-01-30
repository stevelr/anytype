use std::str::FromStr;

use anyhow::{Result, bail};
use anytype::prelude::*;
use serde_json::Number;

pub fn parse_filters(filters: &[String]) -> Result<Vec<Filter>> {
    filters.iter().map(|f| parse_filter(f)).collect()
}

pub fn parse_filter(input: &str) -> Result<Filter> {
    let (left, value) = input
        .split_once('=')
        .ok_or_else(|| anyhow::anyhow!("invalid filter: {input}"))?;

    let (property_key, condition_str) = if let Some((key, rest)) = left.split_once('[') {
        if !rest.ends_with(']') {
            bail!("invalid filter condition: {input}");
        }
        (key.trim(), Some(&rest[..rest.len() - 1]))
    } else {
        (left.trim(), None)
    };

    let condition = parse_condition(condition_str)?;

    if property_key.is_empty() {
        bail!("invalid filter property: {input}");
    }

    let value = value.trim();

    match condition {
        Condition::Empty => Ok(Filter::is_empty(property_key)),
        Condition::NotEmpty => Ok(Filter::not_empty(property_key)),
        Condition::In | Condition::NotIn => {
            let values = split_list(value);
            if property_key == "type" {
                Ok(Filter::Objects {
                    condition,
                    property_key: "type".to_string(),
                    objects: values,
                })
            } else {
                Ok(Filter::MultiSelect {
                    condition,
                    property_key: property_key.to_string(),
                    multi_select: values,
                })
            }
        }
        _ => {
            if let Some(bool_val) = parse_bool(value) {
                return Ok(Filter::Checkbox {
                    condition,
                    property_key: property_key.to_string(),
                    checkbox: bool_val,
                });
            }
            if let Some(number) = parse_number(value) {
                return Ok(Filter::Number {
                    condition,
                    property_key: property_key.to_string(),
                    number,
                });
            }
            Ok(Filter::Text {
                condition,
                property_key: property_key.to_string(),
                text: value.to_string(),
            })
        }
    }
}

pub fn parse_property(input: &str) -> Result<(String, String)> {
    let (left, value) = input
        .split_once('=')
        .ok_or_else(|| anyhow::anyhow!("invalid property: {input}"))?;

    if left.contains(':') {
        bail!("property format is no longer accepted: {input}");
    }
    let key = left.trim();

    if key.is_empty() {
        bail!("invalid property key: {input}");
    }

    Ok((key.to_string(), value.trim().to_string()))
}

pub fn parse_type_property(input: &str) -> Result<CreateTypeProperty> {
    let mut parts = input.splitn(3, ':');
    let key = parts.next().unwrap_or_default().trim();
    let format = parts.next().unwrap_or_default().trim();
    let name = parts.next().unwrap_or_default().trim();

    if key.is_empty() || format.is_empty() || name.is_empty() {
        bail!("invalid type property: {input}");
    }

    let format = PropertyFormat::from_str(format)
        .map_err(|_| anyhow::anyhow!("invalid property format: {format}"))?;

    Ok(CreateTypeProperty {
        key: key.to_string(),
        format,
        name: name.to_string(),
    })
}

fn parse_condition(raw: Option<&str>) -> Result<Condition> {
    let raw = raw.unwrap_or("eq").trim().to_ascii_lowercase();
    let condition = match raw.as_str() {
        "eq" => Condition::Equal,
        "ne" | "neq" => Condition::NotEqual,
        "empty" => Condition::Empty,
        "nempty" => Condition::NotEmpty,
        "lt" => Condition::Less,
        "lte" => Condition::LessOrEqual,
        "gt" => Condition::Greater,
        "gte" => Condition::GreaterOrEqual,
        "contains" => Condition::Contains,
        "ncontains" => Condition::NotContains,
        "in" => Condition::In,
        "nin" => Condition::NotIn,
        _ => bail!("invalid filter condition: {raw}"),
    };
    Ok(condition)
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn parse_number(value: &str) -> Option<Number> {
    if let Ok(num) = value.parse::<i64>() {
        return Some(Number::from(num));
    }
    if let Ok(num) = value.parse::<u64>() {
        return Some(Number::from(num));
    }
    if let Ok(num) = value.parse::<f64>() {
        return Number::from_f64(num);
    }
    None
}

fn split_list(value: &str) -> Vec<String> {
    if value.is_empty() {
        return Vec::new();
    }
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToString::to_string)
        .collect()
}
