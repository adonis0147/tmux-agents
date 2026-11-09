# Installation and releases

See the [README](../README.md) for plugin setup and interactive usage. The public release repository is [`adonis0147/tmux-agents`](https://github.com/adonis0147/tmux-agents).

## Automatic installation

Clone into `~/.tmux/plugins/tmux-agents` and load `tmux-agents.tmux`, either directly or through TPM with `set -g @plugin 'adonis0147/tmux-agents'`. The standard `.tmux` entry point runs `sh scripts/install.sh` before registering the command alias or bindings.

The installer resolves the binary in this order:

1. If `TMUX_AGENTS_BIN` is nonempty, require it to be an absolute path to an executable file and return that path without downloading. An invalid override is an error, not a fallback to a release.
2. Read the exact release tag from the root `RELEASE_TAG` file and select a target using `uname`. The file must contain exactly one line matching `[A-Za-z0-9][A-Za-z0-9._-]*`.
3. Reuse an existing executable at `<plugin>/bin/<tag>/<target>/tmux-agents`, including offline. It is not refreshed or replaced on subsequent loads.
4. Otherwise download the exact tag's raw binary and checksum, verify them, and install locally.

The current release tag is exactly `pre-0.1`. The Cargo package version remains `0.1.0` independently; the installer does not use it for release identification.

Supported release targets:

| Platform | Architecture | Rust target | Build runner |
| --- | --- | --- | --- |
| macOS 10.12+ | x86_64 | `x86_64-apple-darwin` | `macos-15-intel` |
| macOS 11.0+ | aarch64 (Apple Silicon) | `aarch64-apple-darwin` | `macos-15` |
| Linux | x86_64 | `x86_64-unknown-linux-musl` | `ubuntu-24.04` |
| Linux | aarch64 | `aarch64-unknown-linux-musl` | `ubuntu-24.04` |

Linux releases use `cargo-zigbuild` with Zig's musl toolchain for both architectures, rather than a system musl compiler. Both Linux targets cross-build on the x86_64 runner. Zig is a build-time dependency only; users do not need it to install the plugin. Unsupported platforms require a source build and explicit override. tmux 3.8 or newer must still be installed separately. Automatic installation needs no Rust toolchain, sudo, or installation onto PATH, and does not install tmux or start the supervisor.

### Downloads and verification

A fresh download requires `curl` plus either `sha256sum` or `shasum`. For the exact `${tag}` from `RELEASE_TAG` and selected `${target}`, the assets are (no `v` prefix is inserted):

```text
https://github.com/adonis0147/tmux-agents/releases/download/${tag}/tmux-agents-${tag}-${target}
https://github.com/adonis0147/tmux-agents/releases/download/${tag}/SHA256SUMS
```

These are raw executables, not archives. Downloads and redirects are HTTPS-only. A single `SHA256SUMS` manifest covers all four binaries; there are no per-binary `.sha256` assets. The installer requires exactly one matching filename entry with a valid 64-character hexadecimal hash, then compares the binary's SHA-256 digest. Checksums protect integrity, not independent authenticity: the binary and checksum share the same GitHub/repository trust boundary.

Downloads are staged under `<plugin>/bin/<tag>/<target>/.install.XXXXXX`. Only after successful verification is the executable atomically renamed to `<plugin>/bin/<tag>/<target>/tmux-agents`. Concurrent installs may download duplicate verified bytes; publishing them by atomic rename is safe. Existing installed executables are reused rather than routinely replaced or revalidated.

The installer prints only the resolved binary path to stdout; progress and diagnostics go to stderr. It never fetches “latest” and performs no background upgrades.

### Failures and diagnostics

**The exact `pre-0.1` release must exist before automatic installation of the current checkout works.** A checkout whose `RELEASE_TAG` is not yet published may request assets that do not exist. Use a checkout whose `RELEASE_TAG` names a published release, or use a source-build override.

If installation fails, the plugin reports the failure and exits before registering bindings or the `tmux machine` alias. Correct the problem and reload the plugin to retry. To see installer diagnostics directly:

```sh
cd ~/.tmux/plugins/tmux-agents
sh scripts/install.sh
```

Check the override, platform support, download tools, network access, and presence of the exact release assets. A checksum failure leaves the unverified download uninstalled.

## Source builds and offline overrides

Build from a source checkout with Rust/Cargo:

```sh
cd /absolute/repo
cargo build --release
TMUX_AGENTS_BIN=/absolute/repo/target/release/tmux-agents sh scripts/install.sh
```

The override returns the executable path without a download. It can also point to another trusted, locally provisioned executable for offline use.

The environment must exist in the **tmux server before the plugin loads**; exporting it in a pane does not update an already running server. For an existing server:

```sh
tmux set-environment -g TMUX_AGENTS_BIN /absolute/repo/target/release/tmux-agents
tmux run-shell '/absolute/repo/tmux-agents.tmux'
```

For a new server, export the variable before starting tmux and loading the plugin:

```sh
export TMUX_AGENTS_BIN=/absolute/repo/target/release/tmux-agents
tmux
```

To return an existing server to automatic installation, unset the override and reload:

```sh
tmux set-environment -gu TMUX_AGENTS_BIN
tmux run-shell "${HOME}/.tmux/plugins/tmux-agents/tmux-agents.tmux"
```

Direct source commands such as `./target/release/tmux-agents supervisor` and `./target/release/tmux-agents ensure` still work from the repository root.

## Publishing a release

The tag workflow is [`.github/workflows/release.yml`](../.github/workflows/release.yml). It builds all four targets above, using native `macos-15-intel` and `macos-15` runners for Intel and Apple Silicon respectively with deployment targets `10.12` for Intel and `11.0` for Apple Silicon, matching Herdr TUI v0.9.5's binary minimums. The macOS build step checks the produced Mach-O minimum before packaging. These are deployment targets, not confirmation of runtime testing on those older systems. Both Linux targets build on `ubuntu-24.04` using pinned `cargo-zigbuild` 0.24.1 and Zig 0.15.2 (`ziglang`), with `cargo zigbuild --release --locked --target "${TARGET}"`. No ARM runner or system `musl-tools` package is required.

Prerequisites:

- Keep `adonis0147/tmux-agents` public and enable GitHub Actions.
- Allow the publish job's `contents: write` permission. Publishing uses the standard GitHub token (`github.token`), not a separately configured secret.
- Set the root `RELEASE_TAG` file to the intended exact tag. The workflow triggers on `pre-*` and `v*` tags and requires `GITHUB_REF_NAME` to equal `RELEASE_TAG` exactly; tags are not derived from the Cargo package version.

Ensure `origin` points to the public repository. If it is absent, configure it before pushing:

```sh
git remote add origin https://github.com/adonis0147/tmux-agents.git
```

For the initial `pre-0.1` release, after committing the intended source and workflow with `RELEASE_TAG` containing `pre-0.1`:

```sh
cargo test
cargo build --release
python3 tests/installer.py -v
python3 tests/integration.py -v
git push -u origin HEAD
git tag pre-0.1
git push origin pre-0.1
```

The workflow validates the tag against `RELEASE_TAG`, runs Rust unit tests (`cargo test --locked`) and offline installer tests on each runner, and builds release binaries for every target. Cross-compiled binaries are not runtime-tested by this workflow; the full tmux integration suite is a separate local check. Each build packages only `tmux-agents-${tag}-${target}`. The publish job generates one `SHA256SUMS` manifest covering all four binaries.

Only **after all four builds succeed** does the publish job download their artifacts and call `gh release create` with the raw binaries and checksum files, a verified tag, and generated release notes. Tags matching `pre-*` or `v*-*` publish with `--prerelease --latest=false`; stable `v*` releases use standard publishing. Check the Actions run and published assets before directing users to install. The initial `pre-0.1` release workflow passed for all four targets, and its published macOS Intel binary was downloaded, checksum-verified, and executed locally. Linux musl targets were also cross-built locally and confirmed as statically linked ELF executables. Linux runtime testing and testing on the minimum macOS versions remain pending.

## Upgrades and release immutability

Treat published releases as immutable: do not replace binaries or move released tags. For future releases, update `RELEASE_TAG` to a new `pre-*` or `v*` tag, commit the changes, and push that exact tag. Optionally bump the semantic package version in both `Cargo.toml` and `Cargo.lock` when appropriate; release tags need not equal `v` plus that version. The installer pins to the checkout's `RELEASE_TAG`, not the newest GitHub release. Update the plugin checkout and reload it to install that release.

The `pre-0.1` release is being replaced once at the maintainer's explicit request to align macOS minimums and consolidate checksums. Existing cached executables are not automatically refreshed after an overwrite: remove only `<plugin>/bin/pre-0.1/<target>/tmux-agents` and reload the plugin to download the replacement. Do not remove workspace state.

Installation never restarts a running supervisor. A newer CLI does not automatically replace the service; protocol upgrades and any required migration are manual. See [lifecycle and limits](prototype.md#lifecycle-and-limits) before planning an upgrade.
