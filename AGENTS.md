# AGENTS.md

Instructions for AI coding agents working with this codebase.

## Package Manager

This project uses **pnpm**. Always use `pnpm` instead of `npm` or `yarn` for installing dependencies, running scripts, etc. (e.g., `pnpm install`, `pnpm run build`).

## Code Style

- Do not use emojis in code, output, or documentation. Unicode symbols (✓, ✗, →, ⚠) are acceptable.
- In documentation and markdown, never use double hyphens (`--`) as a dash. Use an emdash (—) sparingly when needed. Prefer rewriting the sentence to avoid dashes entirely.
- CLI colored output uses `cli/src/color.rs`. This module respects the `NO_COLOR` environment variable. Never use hardcoded ANSI color codes.
- CLI flags must always use kebab-case (e.g., `--auto-connect`, `--allow-file-access`). Never use camelCase for flags (e.g., `--autoConnect` is wrong).

## Documentation

When adding or changing user-facing features (new flags, commands, behaviors, environment variables, etc.), update **all** of the following:

1. `cli/src/output.rs` — `--help` output (flags list, examples, environment variables)
2. `README.md` — Options table, relevant feature sections, examples
3. `skills/agent-browser/SKILL.md` — so AI agents know about the feature
4. Inline doc comments in the relevant source files

This applies to changes that either human users or AI agents would need to know about. Do not skip any of these locations.

## Releasing

Releases are manual, single-PR affairs. There is no changesets automation. The maintainer controls the changelog voice and format.

To prepare a release:

1. Create a branch (e.g. `prepare-v0.24.0`)
2. Bump `version` in `package.json`
3. Run `pnpm version:sync` to update `cli/Cargo.toml`, `cli/Cargo.lock`, and `packages/dashboard/package.json`
4. Write the changelog entry in `CHANGELOG.md` at the top, under a new `## <version>` heading, wrapped in `<!-- release:start -->` and `<!-- release:end -->` markers
5. Open a PR and merge to `main`

When the PR merges, CI compares `package.json` version to what's on GitHub Packages. If it differs, it builds all 7 platform binaries, publishes to GitHub Packages, and creates the GitHub release automatically. The GitHub release body is extracted from the content between the `<!-- release:start -->` and `<!-- release:end -->` markers in `CHANGELOG.md`.

### Writing the changelog

Review the git log since the last release and write the entry in `CHANGELOG.md`. Follow the existing format and voice. Group changes under `### New Features`, `### Bug Fixes`, `### Improvements`, etc. Bold the feature/fix name, then describe it concisely. Reference PR numbers in parentheses.

Wrap the release notes (everything between the `## <version>` heading and the previous version) in markers so CI can extract them for the GitHub release:

```markdown
## 0.24.0

<!-- release:start -->
### New Features

- **Foo command** - Added `foo` command for bar (#1234)

### Bug Fixes

- Fixed **baz** not working when qux is enabled (#1235)

### Contributors

- @ctate
- @somecontributor
<!-- release:end -->

## 0.23.3
```

Include a `### Contributors` section listing the GitHub usernames (with `@` prefix) of everyone who contributed to the release. Check the git log between the previous tag and HEAD to find them.

Do not prefix entries with commit hashes. Do not use the changesets `### Patch Changes` / `### Minor Changes` headings. Use descriptive section names instead.

### Docs changelog

The docs changelog at `docs/src/app/changelog/page.mdx` mirrors `CHANGELOG.md` but uses a slightly different format. Each entry uses:

- A `v` prefix on the version (e.g. `## v0.24.0`)
- A date line with the full date: `<p className="text-[#888] text-sm">March 30, 2026</p>`
- A `---` separator between entries

Match the existing style in that file.

## Architecture

This is a Rust codebase. The browser automation daemon lives in `cli/src/native/` (daemon, actions, browser, CDP client, snapshot, state). The `--engine` flag selects Chrome vs Lightpanda. The `install` command downloads Chrome from Chrome for Testing directly.

## Testing

### Unit Tests

```bash
cd cli && cargo test
```

Runs all unit tests (~320 tests). These are fast and don't require Chrome.

### End-to-End Tests

```bash
cd cli && cargo test e2e -- --ignored --test-threads=1
```

Runs 18 e2e tests that launch real headless Chrome instances and exercise the full native daemon command pipeline. Requirements:

- Chrome must be installed
- Must run serially (`--test-threads=1`) to avoid Chrome instance contention
- Tests are `#[ignore]`'d so they don't run during normal `cargo test`

The e2e tests live in `cli/src/native/e2e_tests.rs` and cover: launch/close, navigation, snapshots, screenshots, form interaction, cookies, storage, tabs, element queries, viewport/emulation, domain filtering, diff, state management, error handling, and Phase 8 commands.

### Linting and Formatting

```bash
cd cli && cargo fmt -- --check   # Check formatting
cd cli && cargo clippy            # Lint
```
