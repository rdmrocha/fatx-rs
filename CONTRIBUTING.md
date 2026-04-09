# Contributing to fatx-rs

Thank you for your interest in contributing! This document explains how the project is organized and how to submit changes.

## Branching Model

| Branch | Purpose | Who sees it |
|--------|---------|-------------|
| `main` | Stable releases only | Everyone (default branch on GitHub) |
| `develop` | Active development | Contributors |

**If you're a user:** use the `main` branch. It contains tested, stable releases.

**If you're a contributor:** branch off `develop`, submit PRs targeting `develop`. Changes are merged to `main` only at release time.

## How to Contribute

1. Fork the repository
2. Create a feature branch from `develop`: `git checkout -b my-feature develop`
3. Make your changes
4. Run the test suite: `cargo test --workspace`
5. Submit a pull request targeting the `develop` branch

## Development Setup

```bash
git clone https://github.com/joshuareisbord/fatx-rs.git
cd fatx-rs
git checkout develop
bash setup.sh
```

## Testing

All changes must pass the full test suite (125+ tests):

```bash
cargo test --workspace
```

For manual testing with a physical Xbox 360 drive:

```bash
cargo build --release
sudo ./target/release/fatx scan /dev/rdiskN
sudo ./target/release/fatx ls /dev/rdiskN --partition "360 Data" /
```

## Code Style

- Run `cargo fmt` before committing
- Run `cargo clippy` and address warnings
- Follow existing patterns in the codebase

## License

By contributing, you agree that your contributions will be licensed under the Apache License 2.0, the same license as the project.
