# Contributing to Logos Blockchain Node

We're glad you're interested in contributing to Logos Blockchain Node!

This document describes the guidelines for contributing to the project. We will be updating it as we grow and we figure
out what works best for us.

If you have any questions, come say hi to our [Discord](https://discord.gg/G6q8FgZq)!

## Running Pre-Commit Hooks

To ensure consistent code quality, we ask that you run the `pre-commit` hooks before making a commit.

These hooks help catch common issues early and applies common code style rules, making the review process smoother for
everyone.

1. Install the [pre-commit](https://pre-commit.com/) tool if you haven't already:

```bash
# On Fedora
sudo dnf install pre-commit

# On other systems
pip install pre-commit
```

2. Install the pre-commit hooks:

```bash
pre-commit install
```

3. That's it! The pre-commit hooks will now run automatically when you make a commit.

## Logging Guidelines

Use log levels consistently so logs are easier to read and easier to filter by target.

- Log entries should use the correct target for the subsystem they belong to, and new logs should follow the existing target structure.

- `error`
  - Use when something failed that should normally work.
  - If this log appears, someone should probably look at it.

- `warn`
  - Use when something is off, missing, or degraded, but the node can still continue.
  - If this log appears occasionally, it may be acceptable; if it repeats, it is probably a problem.

- `info`
  - Use for important events that explain the node's current state or major transitions.
  - Someone reading only `info` logs should still understand whether the node is starting, syncing, progressing, changing epoch/session, proposing blocks, or hitting notable conditions.

- `debug`
  - Use for details that explain why the node behaved a certain way.
  - These logs should help answer follow-up questions such as why a decision was taken, why something was skipped, which peers were selected, or why a block was treated a certain way.

- `trace`
  - Use for step-by-step internal flow or very frequent events.
  - These logs should only be needed when doing a deep dive into one specific area.
