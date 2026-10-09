use super::*;

pub(super) fn security_audit_source_files(repository_root: &Path) -> Result<Vec<Value>, String> {
    [
        "tests/cucumber_tests/features/security_audit_v1.feature",
        "tests/src/cucumber/steps/security_audit/mod.rs",
        "tests/src/cucumber/steps/security_audit/brief.rs",
        "tests/src/cucumber/steps/security_audit/producer.rs",
        "tests/src/cucumber/steps/security_audit/sustained/mod.rs",
        "tests/src/cucumber/steps/security_audit/ancestry.rs",
        "tests/src/cucumber/steps/security_audit/scenario.rs",
        "tests/src/cucumber/steps/security_audit/evidence.rs",
        "tests/src/cucumber/steps/security_audit/propagation.rs",
        "tests/src/cucumber/steps/security_audit/sustained/active.rs",
        "tests/tf_integration/logos/src/local/provisioning.rs",
    ]
    .into_iter()
    .map(|relative_path| {
        Ok(json!({
            "path": relative_path,
            "sha256": sha256_file(&repository_root.join(relative_path))?,
        }))
    })
    .collect()
}

/// Locate the original logs retained in each participant's runtime directory.
pub(super) fn original_node_logs(world: &CucumberWorld) -> Result<Vec<String>, String> {
    let participants = audit_participants(world)?;
    let mut paths = Vec::new();
    for node_name in std::iter::once(&participants.producer).chain(&participants.honest_nodes) {
        let node = world
            .nodes_info
            .get(node_name)
            .ok_or_else(|| format!("node {node_name} was not started"))?;
        let prefix = format!("__logs-{}", node.runtime_name);
        let mut node_paths = Vec::new();
        for entry in fs::read_dir(&node.runtime_dir)
            .map_err(|error| format!("cannot inspect {}: {error}", node.runtime_dir.display()))?
        {
            let entry = entry.map_err(|error| error.to_string())?;
            if !entry
                .file_type()
                .map_err(|error| error.to_string())?
                .is_dir()
                && entry.file_name().to_string_lossy().starts_with(&prefix)
            {
                File::open(entry.path()).map_err(|error| error.to_string())?;
                node_paths.push(entry.path());
            }
        }
        if node_paths.is_empty() {
            return Err(format!("no original log file found for {node_name}"));
        }
        node_paths.sort();
        paths.extend(
            node_paths
                .into_iter()
                .map(|path| path.display().to_string()),
        );
    }
    Ok(paths)
}

pub(super) fn parse_log_fields(line: &str) -> HashMap<String, String> {
    let mut fields = line
        .split_whitespace()
        .filter_map(|field| field.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.trim_matches('"').to_owned()))
        .collect::<HashMap<_, _>>();
    if let Some(reason) = parse_quoted_log_field(line, "reason") {
        fields.insert("reason".to_owned(), reason);
    }
    fields
}

pub(super) fn parse_quoted_log_field(line: &str, key: &str) -> Option<String> {
    let marker = format!("{key}=");
    let (start, _) = line.match_indices(&marker).find(|(index, _)| {
        *index == 0
            || line
                .get(..*index)
                .is_some_and(|prefix| prefix.chars().next_back().is_some_and(char::is_whitespace))
    })?;
    let mut value = line.get(start + marker.len()..)?.chars();
    if value.next()? != '"' {
        return None;
    }
    let mut output = String::new();
    let mut escaped = false;
    for character in value {
        if escaped {
            output.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == '"' {
            return Some(output);
        } else {
            output.push(character);
        }
    }
    None
}

pub(super) fn parse_log_timestamp(line: &str) -> Option<[u32; 7]> {
    let stamp = line
        .split_whitespace()
        .next()?
        .trim_start_matches('[')
        .trim_end_matches(']');
    let stamp = stamp.strip_suffix('Z')?;
    let (date, time) = stamp.split_once('T')?;
    let mut date_parts = date.split('-').map(str::parse::<u32>);
    let year = date_parts.next()?.ok()?;
    let month = date_parts.next()?.ok()?;
    let day = date_parts.next()?.ok()?;
    if date_parts.next().is_some() {
        return None;
    }
    let (clock, fraction) = time.split_once('.').unwrap_or((time, ""));
    let mut time_parts = clock.split(':').map(str::parse::<u32>);
    let hour = time_parts.next()?.ok()?;
    let minute = time_parts.next()?.ok()?;
    let second = time_parts.next()?.ok()?;
    if time_parts.next().is_some() || fraction.len() > 9 {
        return None;
    }
    let nanos = if fraction.is_empty() {
        0
    } else {
        let fractional = fraction.parse::<u32>().ok()?;
        fractional.checked_mul(10u32.checked_pow((9 - fraction.len()) as u32)?)?
    };
    Some([year, month, day, hour, minute, second, nanos])
}

pub(super) fn format_log_timestamp(timestamp: Option<[u32; 7]>) -> Option<String> {
    let [year, month, day, hour, minute, second, nanos] = timestamp?;
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{nanos:09}Z"
    ))
}

pub(super) fn parse_usize(fields: &HashMap<String, String>, key: &str) -> Option<usize> {
    fields.get(key)?.parse().ok()
}

pub(super) fn identity_bool(fields: &HashMap<String, String>, key: &str) -> bool {
    fields.get(key).is_some_and(|value| value == "true")
}

pub(super) fn sha256_file(path: &Path) -> Result<String, String> {
    let output = Command::new("sha256sum")
        .arg(path)
        .output()
        .map_err(|error| format!("cannot hash {} with sha256sum: {error}", path.display()))?;
    if !output.status.success() {
        return Err(format!("sha256sum failed for {}", path.display()));
    }
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next()
        .map(str::to_owned)
        .ok_or_else(|| format!("sha256sum returned no digest for {}", path.display()))
}

pub(super) fn command_output(command: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(command)
        .args(args)
        .output()
        .map_err(|error| format!("cannot run {command}: {error}"))?;
    if !output.status.success() {
        return Err(format!("{command} {} failed", args.join(" ")));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
