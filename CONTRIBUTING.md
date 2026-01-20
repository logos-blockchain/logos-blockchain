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
