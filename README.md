# reg-wiki

A living wiki for agents: one global wiki of linked Markdown pages that agents
write to directly, with git as the review. See [PLAN.md](PLAN.md) for the design.

## Install

Linux (x86_64, aarch64; static musl builds) and macOS (Apple silicon, Intel):

```sh
curl -fsSL https://raw.githubusercontent.com/HuxleyMc/reg-wiki/main/install.sh | sh
```

This installs `reg-wiki` to `~/.local/bin`, checking the SHA-256 of the download.
Then run `reg-wiki install` to wire it into Claude Code, or
`reg-wiki install --target codex,cursor,gemini` for other agents, which reach the wiki
through its MCP server (`reg-wiki serve --mcp`). `reg-wiki ai schedule weekly` has the
librarian tend the wiki on a schedule.

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

Bump `version` in `Cargo.toml` (run `cargo check` to update `Cargo.lock`) and push to `main`.
Once CI passes, it sees that `v<version>` has no release yet and publishes one from
the binaries it just built: tarballs, `.sha256` files, `SHA256SUMS` and `install.sh`.
Pushes that don't change the version don't release. Versions with a suffix
(`0.2.0-rc.1`) become pre-releases.

Pushing a `v*` tag, or running the **release** workflow by hand, also publishes a release
(`.github/workflows/release.yml`).
