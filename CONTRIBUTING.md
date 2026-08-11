# Contributing to LCode

First off, thank you for considering contributing to LCode! We welcome
contributions of all kinds: bug fixes, new features, documentation, tests, and
more.

## Table of Contents

- [Development Environment](#development-environment)
- [Development Workflow](#development-workflow)
- [Code Style](#code-style)
- [Testing](#testing)
- [Commit Message Guidelines](#commit-message-guidelines)
- [Branch Naming](#branch-naming)

## Development Environment

### Prerequisites

- **Rust 1.94 or later** — install via [rustup](https://rustup.rs)
- **Git** — for cloning the repository and managing changes

### Getting Started

```bash
# Clone the repository
git clone https://github.com/Lixiang9716/LCode.git
cd LCode

# Build the project
cargo build

# Run the test suite
cargo test
```

To install Rust if you don't have it yet:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup update stable
```

## Development Workflow

We use a standard fork-and-PR workflow:

1. **Fork** the repository on GitHub.
2. **Create a branch** from `main` for your work (see [Branch Naming](#branch-naming)).
3. **Develop** your changes, following the [Code Style](#code-style) and
   [Testing](#testing) guidelines.
4. **Test** locally before pushing — make sure the full test suite passes.
5. **Open a pull request** against the `main` branch using the
   [pull request template](.github/PULL_REQUEST_TEMPLATE.md).

### Before Opening a PR

- Run `cargo fmt` to format your code.
- Run `cargo clippy -- -D warnings` and fix any warnings.
- Run `cargo test --all-features` and make sure everything passes.
- Update documentation (README, doc comments, etc.) if your change affects it.
- Update the [CHANGELOG.md](CHANGELOG.md) if your change is user-visible.

## Code Style

- Format code with **rustfmt**: `cargo fmt`
- Lint with **clippy** — your code must pass with warnings treated as errors:

  ```bash
  cargo clippy -- -D warnings
  ```

- Follow the standard Rust naming conventions:
  - `snake_case` for functions, methods, variables, and modules
  - `CamelCase` for types, traits, and enums
  - `SCREAMING_SNAKE_CASE` for constants and statics
- Keep functions focused and small; prefer clear, descriptive names over
  clever ones.
- Add doc comments (`///`) to public items.

## Testing

- **Every PR must include tests** for the code it adds or changes.
- The full test suite must pass with all features enabled:

  ```bash
  cargo test --all-features
  ```

- When fixing a bug, add a test that reproduces the bug before fixing it,
  then verify the test passes with your fix.
- When adding a feature, include tests covering the main behavior and edge cases.

## Commit Message Guidelines

We use [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <description>
```

Allowed types:

| Type       | Description                                        |
|------------|----------------------------------------------------|
| `feat`     | A new feature                                      |
| `fix`      | A bug fix                                          |
| `chore`    | Maintenance tasks (deps, tooling, etc.)            |
| `docs`     | Documentation only changes                         |
| `refactor` | Code changes that neither fix a bug nor add a feature |
| `test`     | Adding or updating tests                           |
| `ci`       | Changes to CI configuration or scripts             |

Examples:

```
feat(tools): add list_dir tool for directory listings
fix(repl): handle empty input gracefully
docs(readme): update installation instructions
test(agent): add unit tests for memory module
ci: pin rust toolchain version in workflow
```

A scope (e.g. `(agent)`, `(repl)`, `(config)`) is optional but encouraged
when the change is localized to a module.

## Branch Naming

Use a descriptive prefix followed by a short summary of the work:

| Prefix     | Purpose                          | Example                          |
|------------|----------------------------------|----------------------------------|
| `feature/` | New features                     | `feature/add-list-dir-tool`      |
| `fix/`     | Bug fixes                        | `fix/repl-empty-input-panic`     |
| `task/`    | Maintenance or project work      | `task/governance-files`          |

Branch names should be lowercase, use hyphens to separate words, and be
concise but descriptive.

## Release Process

Releases are automated via **release-please** (`.github/workflows/release.yml`):

1. Every push to `main` is scanned for Conventional Commits
2. Version bumps are derived from commit summaries:
   - `feat:` → minor bump (e.g. `0.1.0` → `0.2.0`)
   - `fix:` → patch bump (e.g. `0.1.0` → `0.1.1`)
   - `BREAKING CHANGE:` footer → major bump (e.g. `0.x` → `1.0.0`)
3. release-please opens a release PR updating `Cargo.toml` + `CHANGELOG.md`
4. After the release PR is merged, a git tag and GitHub Release are created,
   and prebuilt binaries for Linux/macOS/Windows are attached automatically

**Important**: use Conventional Commits on every commit destined for `main`,
or the version bump will be wrong (a `chore:` commit produces no release).

## Questions?

If you have questions or need help, open an issue or reach out to the
maintainers on the project's GitHub page.
