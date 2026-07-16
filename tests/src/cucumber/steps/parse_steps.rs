use cucumber::gherkin::Step;

use crate::cucumber::error::StepError;

pub fn parse_required_string_cell(value: &str, name: &str, tag: &str) -> Result<String, StepError> {
    let value = value.trim();

    if value.is_empty() {
        return Err(StepError::InvalidArgument {
            message: format!("{tag} resources `{name}` cannot be empty"),
        });
    }

    Ok(value.to_owned())
}

#[must_use]
pub fn optional_string_cell(value: &str) -> Option<String> {
    let value = value.trim();

    (!value.is_empty()).then(|| value.to_owned())
}

pub fn invalid_table_row<T>(
    description: &str,
    headers: &[&str],
    actual_columns: usize,
) -> Result<T, StepError> {
    Err(StepError::InvalidArgument {
        message: format!(
            "{description} resources rows must have exactly {} columns (`{}`), got {actual_columns}",
            headers.len(),
            headers.join("`, `"),
        ),
    })
}

pub fn parse_table_rows<T>(
    step: &Step,
    headers: &[&str],
    description: &str,
    parse_row: impl Fn(&[String]) -> Result<T, StepError>,
) -> Result<Vec<T>, StepError> {
    let table = step.table.as_ref().ok_or(StepError::MissingTable)?;

    if table.rows.is_empty() {
        return Err(StepError::InvalidArgument {
            message: format!("{description} resources must include a header row"),
        });
    }

    let header = table.rows[0].iter().map(String::as_str).collect::<Vec<_>>();
    if header.as_slice() != headers {
        return Err(StepError::InvalidArgument {
            message: format!(
                "{description} resources must use `{}` header",
                headers.join("`, `")
            ),
        });
    }

    table
        .rows
        .iter()
        .skip(1)
        .map(|row| parse_row(row))
        .collect()
}

pub fn parse_list_cell(value: &str, column: &str, tag: &str) -> Result<Vec<String>, StepError> {
    let values = value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();

    if values.is_empty() {
        return Err(StepError::InvalidArgument {
            message: format!("{tag} `{column}` must list at least one alias"),
        });
    }

    Ok(values)
}

pub fn single_column_table(
    step: &Step,
    header_name: &str,
    description: &str,
) -> Result<Vec<String>, StepError> {
    parse_table_rows(step, &[header_name], description, |row| match row {
        [value] => Ok(value.clone()),
        _ => invalid_table_row(description, &[header_name], row.len()),
    })
}

pub fn parse_single_table_row<T>(
    step: &Step,
    headers: &[&str],
    description: &str,
    parse_row: impl Fn(&[String]) -> Result<T, StepError>,
) -> Result<T, StepError> {
    let mut rows = parse_table_rows(step, headers, description, parse_row)?;
    if rows.len() != 1 {
        return Err(StepError::InvalidArgument {
            message: format!("{description} must contain exactly one data row"),
        });
    }

    Ok(rows.remove(0))
}

pub fn parse_required_bool_cell(value: &str, column: &str, tag: &str) -> Result<bool, StepError> {
    match value.to_lowercase().trim() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(StepError::InvalidArgument {
            message: format!("{tag} `{column}` must be `true` or `false`"),
        }),
    }
}

pub fn parse_usize_cell(value: &str, column: &str) -> Result<usize, StepError> {
    value.parse().map_err(|error| StepError::InvalidArgument {
        message: format!("Invalid `{column}` value `{value}`: {error}"),
    })
}

pub fn parse_u32_cell(value: &str, column: &str) -> Result<u32, StepError> {
    value.parse().map_err(|error| StepError::InvalidArgument {
        message: format!("Invalid `{column}` value `{value}`: {error}"),
    })
}

pub fn parse_u64_cell(value: &str, column: &str) -> Result<u64, StepError> {
    value.parse().map_err(|error| StepError::InvalidArgument {
        message: format!("Invalid `{column}` value `{value}`: {error}"),
    })
}

pub fn comma_separated_aliases(
    value: &str,
    name: &str,
    tag: &str,
) -> Result<Vec<String>, StepError> {
    let aliases = value
        .split(',')
        .map(str::trim)
        .filter(|alias| !alias.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();

    if aliases.is_empty() {
        return Err(StepError::InvalidArgument {
            message: format!("{tag} resources `{name}` must list at least one alias"),
        });
    }

    Ok(aliases)
}
