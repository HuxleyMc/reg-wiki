# reg-wiki

A living wiki for agents: one global wiki of linked Markdown pages that agents
write to directly, with git as the review. See [PLAN.md](PLAN.md) for the design.

## Install

Linux (x86_64, aarch64; static musl builds) and macOS (Apple silicon, Intel):

```sh
curl -fsSL https://raw.githubusercontent.com/HuxleyMc/reg-wiki/main/install.sh | sh
```

This installs `reg-wiki` to `~/.local/bin`, checking the SHA-256 of the download.
Then run `reg-wiki install` to wire it into your agents.

**Update:** run the same command again. It does nothing if you're already on the latest release.

Options (pass after `sh -s --`):

```sh
curl -fsSL .../install.sh | sh -s -- --version v0.2.0   # pin a release
curl -fsSL .../install.sh | sh -s -- --dir /usr/local/bin
curl -fsSL .../install.sh | sh -s -- --uninstall        # run `reg-wiki uninstall` first
```

`REG_WIKI_VERSION` and `REG_WIKI_INSTALL_DIR` work as environment variables too.

From source: `cargo install --locked --git https://github.com/HuxleyMc/reg-wiki`.

## Development

CI (`.github/workflows/ci.yml`) runs fmt, clippy and tests on every PR and push
to `main`. Pushes to `main` also build release binaries for every target, which
you can download as workflow artifacts.

### Releasing

1. Bump `version` in `Cargo.toml`, run `cargo check` to update `Cargo.lock`, and commit to `main`.
2. Tag and push: `git tag v0.2.0 && git push origin v0.2.0`.

`.github/workflows/release.yml` checks that the tag matches `Cargo.toml`, builds
every target, and publishes a GitHub release with tarballs, `.sha256` files,
`SHA256SUMS` and `install.sh`. Tags with a suffix (`v0.2.0-rc.1`) become pre-releases.
