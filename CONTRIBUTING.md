# Contributing

Thanks for helping improve `chat-responses-codex`.

## Before You Send Changes

- Work from the `main` branch.
- Keep GitHub as the source of truth and let Gitee mirror the same commits.
- Run `rtk cargo fmt --all`.
- Run `rtk cargo test`.
- Update documentation when you change user-visible behavior, config keys, ports, image names, or repository names.

## Suggested Workflow

1. Create a feature branch.
2. Make the code change.
3. Add or update tests.
4. Update docs if the user experience changed.
5. Run the formatter and test suite.
6. Open a pull request against `main`.

## Style

- Prefer small, focused changes.
- Keep public-facing names and docs consistent across README, deployment docs, examples, and tests.
- Do not revert unrelated changes in the working tree.

## Release And Mirror Sync

- Merge to GitHub `main` first.
- Mirror the same commit to Gitee after GitHub passes.
- Push tags to both remotes when you cut a release.

