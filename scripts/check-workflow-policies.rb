#!/usr/bin/env ruby
# frozen_string_literal: true

# Enforces the policy of the GitHub workflows and local actions on the CI token and on the code they run.
#
# Checks:
# - Every workflow sets top-level `permissions` that grant no write scope. A job without its own
#   `permissions` then gets a read-only token, and write access is granted only by the jobs that need it.
# - Every `actions/checkout` sets `persist-credentials`: `false`, or `true` in a job that pushes. Otherwise
#   the token stays in the Git configuration, where every later step can read it, and so can anything those
#   steps build.
# - Every action and reusable workflow from another repository is pinned to a full commit SHA, which,
#   unlike a tag, cannot be moved to other code.
#
# Written in Ruby because its standard library parses YAML, and Ruby comes with GitHub's runners and macOS.
#
# Exit codes:
# - 0: all checks passed.
# - 1: one or more policy violations were found, or a file could not be parsed.

require 'psych'

ROOT = File.expand_path('..', __dir__)
COMMIT_SHA = /@[0-9a-f]{40}\z/.freeze
IMAGE_DIGEST = /@sha256:[0-9a-f]{64}\z/.freeze

def relative(path)
  path.delete_prefix("#{ROOT}/")
end

def load_yaml(path)
  Psych.safe_load(File.read(path), aliases: true)
rescue Psych::Exception => error
  abort "error: #{relative(path)}: #{error.message}"
end

def check_uses(uses, where, violations)
  return if uses.nil? || uses.start_with?('./')

  if uses.start_with?('docker://')
    violations << "#{where}: `#{uses}` is not pinned to an image digest" unless IMAGE_DIGEST.match?(uses)
  elsif !COMMIT_SHA.match?(uses)
    violations << "#{where}: `#{uses}` is not pinned to a full commit SHA"
  end
end

def check_steps(steps, where, violations)
  Array(steps).each_with_index do |step, index|
    step_where = "#{where}, step `#{step['name'] || step['uses'] || index + 1}`"
    uses = step['uses']
    check_uses(uses, step_where, violations)
    next unless uses.to_s.start_with?('actions/checkout@')

    persist = (step['with'] || {})['persist-credentials']
    next if [true, false, 'true', 'false'].include?(persist)

    violations << "#{step_where}: set `persist-credentials`, to `false` unless the job pushes"
  end
end

violations = []

Dir[File.join(ROOT, '.github/workflows/*.{yml,yaml}')].sort.each do |path|
  file = relative(path)
  workflow = load_yaml(path)

  permissions = workflow['permissions']
  if permissions.nil?
    violations << "#{file}: no top-level `permissions`; set `contents: read` and grant more in the jobs that need it"
  elsif !permissions.is_a?(Hash)
    violations << "#{file}: list the top-level `permissions` scopes instead of `#{permissions}`"
  else
    writes = permissions.select { |_, level| level == 'write' }.keys
    unless writes.empty?
      violations << "#{file}: the top-level `permissions` grant write (#{writes.join(', ')}); grant it in the jobs that need it"
    end
  end

  (workflow['jobs'] || {}).each do |id, job|
    check_uses(job['uses'], "#{file}: job `#{id}`", violations)
    check_steps(job['steps'], "#{file}: job `#{id}`", violations)
  end
end

Dir[File.join(ROOT, '.github/actions/*/action.{yml,yaml}')].sort.each do |path|
  action = load_yaml(path)
  check_steps((action['runs'] || {})['steps'], relative(path), violations)
end

if violations.empty?
  puts 'All workflows and local actions follow the policy.'
else
  violations.each { |violation| warn "error: #{violation}" }
  exit 1
end
