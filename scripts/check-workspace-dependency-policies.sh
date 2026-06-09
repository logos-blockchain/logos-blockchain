#!/usr/bin/env sh
set -eu

# Enforces workspace dependency and lint policy across Cargo manifests.
#
# Checks:
# - Workspace members must declare dependencies using `workspace = true` only
#   (no direct version/path/git dependency declarations).
# - Workspace members must not override `default-features` on workspace deps.
# - Root `[workspace.dependencies]` entries must set `default-features = false`.
# - Root `[workspace.dependencies]` entries must not set `features`.
# - Workspace members must inherit lints from the workspace via a `[lints]`
#   section containing `workspace = true`.
#
# Exit codes:
# - 0: all checks passed.
# - 1: one or more policy violations were found.
# - 255: internal/tooling error while running the checker.

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
root_manifest="$repo_root/Cargo.toml"

FAILURE_DETECTED_EXIT_CODE=1
INTERNAL_ERROR_EXIT_CODE=255

if [ ! -f "$root_manifest" ]; then
  echo "error: could not find workspace root Cargo.toml at $root_manifest" >&2
  exit $INTERNAL_ERROR_EXIT_CODE
fi

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo is required" >&2
  exit $INTERNAL_ERROR_EXIT_CODE
fi

manifest_list_file="$(mktemp "$repo_root/.tmp-workspace-manifests.XXXXXX")"
awk_script_file="$(mktemp "$repo_root/.tmp-check-workspace-deps.XXXXXX.awk")"

cleanup() {
  rm -f "$manifest_list_file" "$awk_script_file"
}
trap cleanup 0 HUP INT TERM

if ! (
  cd "$repo_root" && cargo metadata --no-deps --format-version 1
) | grep -o '"manifest_path":"[^"]*"' \
  | sed -E 's/^"manifest_path":"(.*)"$/\1/' \
  | sed 's#\\/#/#g' \
  | sort -u >"$manifest_list_file"; then
  echo "error: failed to read workspace manifests via cargo metadata" >&2
  exit $INTERNAL_ERROR_EXIT_CODE
fi

if [ ! -s "$manifest_list_file" ]; then
  echo "error: could not extract package manifests from cargo metadata" >&2
  exit $INTERNAL_ERROR_EXIT_CODE
fi

root_present=0
while IFS= read -r manifest; do
  if [ "$manifest" = "$root_manifest" ]; then
    root_present=1
    break
  fi
done <"$manifest_list_file"
if [ "$root_present" -eq 0 ]; then
  printf '%s\n' "$root_manifest" >>"$manifest_list_file"
fi

cat <<'AWK' >"$awk_script_file"
function trim(s) {
  sub(/^[[:space:]]+/, "", s)
  sub(/[[:space:]]+$/, "", s)
  return s
}

function normalize_dep_name(name) {
  name = trim(name)
  gsub(/^"|"$/, "", name)
  gsub(/^'|'$/, "", name)
  return name
}

function record_violation(rule, dep, line_no, details) {
  violations += 1
  printf("- [%s] %s:%d dependency=%s %s\n", rule, manifest_path, line_no, dep, details)
}

function record_manifest_violation(rule, details) {
  violations += 1
  printf("- [%s] %s %s\n", rule, manifest_path, details)
}

function braces_delta(s,    opens, closes, copy) {
  copy = s
  opens = gsub(/\{/, "{", copy)
  copy = s
  closes = gsub(/\}/, "}", copy)
  return opens - closes
}

function has_key(text, key) {
  return text ~ ("(^|[,{[:space:]])" key "[[:space:]]*=")
}

function has_workspace_true(text) {
  return text ~ /workspace[[:space:]]*=[[:space:]]*true/
}

function has_default_features_false(text) {
  return text ~ /default-features[[:space:]]*=[[:space:]]*false/
}

function in_member_dependency_table(section_name) {
  return section_name ~ /(^|\.)dependencies$/ ||
    section_name ~ /(^|\.)dev-dependencies$/ ||
    section_name ~ /(^|\.)build-dependencies$/
}

function in_member_dependency_item_table(section_name) {
  return section_name ~ /(^|\.)dependencies\.[^\.]+$/ ||
    section_name ~ /(^|\.)dev-dependencies\.[^\.]+$/ ||
    section_name ~ /(^|\.)build-dependencies\.[^\.]+$/
}

function is_workspace_dependencies_table(section_name) {
  return section_name == "workspace.dependencies"
}

function is_workspace_dependency_item_table(section_name) {
  return section_name ~ /^workspace\.dependencies\.[^\.]+$/
}

function reset_specific_state() {
  specific_active = 0
  specific_kind = ""
  specific_dep = ""
  specific_dep_line = 0
  specific_workspace_true = 0
  specific_default_features_seen = 0
  specific_default_features_false = 0
  specific_features_seen = 0
}

function finalize_specific_state() {
  if (!specific_active) {
    return
  }

  if (specific_kind == "member") {
    if (!specific_workspace_true) {
      record_violation("workspace-only", specific_dep, specific_dep_line, "must set workspace = true")
    }
    if (specific_workspace_true && specific_default_features_seen) {
      record_violation("no-default-features-override", specific_dep, specific_dep_line, "must not set default-features when using workspace dependency")
    }
  } else if (specific_kind == "workspace-root") {
    if (!specific_default_features_seen || !specific_default_features_false) {
      record_violation("workspace-default-features-false", specific_dep, specific_dep_line, "must set default-features = false")
    }
    if (specific_features_seen) {
      record_violation("no-features", specific_dep, specific_dep_line, "must not set features")
    }
  }

  reset_specific_state()
}

function analyze_dependency_assignment(dep, value_text, line_no) {
  dep = normalize_dep_name(dep)

  if (current_section == "workspace.dependencies" && is_root_manifest) {
    if (!has_default_features_false(value_text)) {
      record_violation("workspace-default-features-false", dep, line_no, "must set default-features = false")
    }
    if (has_key(value_text, "features")) {
      record_violation("no-features", dep, line_no, "must not set features")
    }
    return
  }

  if (!is_root_manifest && in_member_dependency_table(current_section)) {
    workspace_true = has_workspace_true(value_text)
    default_features_set = has_key(value_text, "default-features")

    if (!workspace_true) {
      record_violation("workspace-only", dep, line_no, "must use workspace = true (no direct dependency declarations)")
    }
    if (workspace_true && default_features_set) {
      record_violation("no-default-features-override", dep, line_no, "must not set default-features when using workspace dependency")
    }
  }
}

BEGIN {
  violations = 0
  current_section = ""
  lints_workspace_true = 0
  reset_specific_state()
}

{
  original_line = $0
  line = $0
  sub(/[[:space:]]*#.*$/, "", line)

  if (line ~ /^[[:space:]]*\[\[[^]]+\]\][[:space:]]*$/) {
    finalize_specific_state()
    section_value = line
    sub(/^[[:space:]]*\[\[/, "", section_value)
    sub(/\]\][[:space:]]*$/, "", section_value)
    current_section = trim(section_value)
    next
  }

  if (line ~ /^[[:space:]]*\[[^]]+\][[:space:]]*$/) {
    finalize_specific_state()
    section_value = line
    sub(/^[[:space:]]*\[/, "", section_value)
    sub(/\][[:space:]]*$/, "", section_value)
    current_section = trim(section_value)

    if (!is_root_manifest && in_member_dependency_item_table(current_section)) {
      specific_active = 1
      specific_kind = "member"
      specific_dep = normalize_dep_name(current_section)
      sub(/^.*\./, "", specific_dep)
      specific_dep = normalize_dep_name(specific_dep)
      specific_dep_line = NR
      next
    }

    if (is_root_manifest && is_workspace_dependency_item_table(current_section)) {
      specific_active = 1
      specific_kind = "workspace-root"
      specific_dep = normalize_dep_name(current_section)
      sub(/^.*\./, "", specific_dep)
      specific_dep = normalize_dep_name(specific_dep)
      specific_dep_line = NR
      next
    }

    next
  }

  if (specific_active) {
    eq_pos = index(line, "=")
    if (eq_pos > 0) {
      key = trim(substr(line, 1, eq_pos - 1))
      value = trim(substr(line, eq_pos + 1))

      if (key == "workspace" && value ~ /^true([[:space:]]|$)/) {
        specific_workspace_true = 1
      }
      if (key == "default-features") {
        specific_default_features_seen = 1
        if (value ~ /^false([[:space:]]|$)/) {
          specific_default_features_false = 1
        }
      }
      if (key == "features") {
        specific_features_seen = 1
      }
    }
    next
  }

  if (!is_root_manifest && current_section == "lints") {
    eq_pos = index(line, "=")
    if (eq_pos > 0) {
      key = trim(substr(line, 1, eq_pos - 1))
      value = trim(substr(line, eq_pos + 1))
      if (key == "workspace" && value ~ /^true([[:space:]]|$)/) {
        lints_workspace_true = 1
      }
    }
  }

  if ((is_root_manifest && current_section == "workspace.dependencies") ||
      (!is_root_manifest && in_member_dependency_table(current_section))) {
    eq_pos = index(line, "=")
    if (eq_pos == 0) {
      next
    }

    dep = trim(substr(line, 1, eq_pos - 1))
    value = trim(substr(line, eq_pos + 1))
    start_line = NR

    if (value ~ /^\{/) {
      buffer = value
      delta = braces_delta(value)
      while (delta > 0 && getline next_line) {
        NR += 0
        temp = next_line
        sub(/[[:space:]]*#.*$/, "", temp)
        buffer = buffer " " trim(temp)
        delta += braces_delta(temp)
      }
      analyze_dependency_assignment(dep, buffer, start_line)
    } else {
      analyze_dependency_assignment(dep, value, start_line)
    }
  }
}

END {
  finalize_specific_state()
  if (violations > 0) {
    exit 2
  }
}
AWK

had_violations=0
had_internal_error=0

while IFS= read -r manifest; do
  is_root=0
  if [ "$manifest" = "$root_manifest" ]; then
    is_root=1
  fi

  if output="$(awk -v manifest_path="$manifest" -v is_root_manifest="$is_root" -f "$awk_script_file" "$manifest")"; then
    :
  else
    status=$?
    if [ "$status" -eq 2 ]; then
      had_violations=1
      if [ -n "$output" ]; then
        printf '%s\n' "$output"
      fi
    else
      had_internal_error=1
      echo "error: failed while checking $manifest" >&2
    fi
  fi
done <"$manifest_list_file"

if [ "$had_internal_error" -eq 1 ]; then
  exit $INTERNAL_ERROR_EXIT_CODE
fi

if [ "$had_violations" -eq 1 ]; then
  exit $FAILURE_DETECTED_EXIT_CODE
fi

echo "workspace dependency policy check passed"
