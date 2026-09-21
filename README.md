<p align="center">
  <img src="https://raw.githubusercontent.com/JJLiebig/codex/main/.github/codex-plus-plus-splash.png" alt="Codex++ splash" width="100%" />
</p>

# Codex++

Codex++ is a lean fork of upstream [OpenAI Codex](https://github.com/openai/codex), focused on multi-account workflows, safer automation, and reliability improvements for long-running sessions.

## Changes vs Upstream

- Multi-account support
  - Add ChatGPT accounts with `codex account add`.
  - Chooses a suitable account on startup based on remaining usage and whether another Codex++ session is already using it. Or chose an account yourself!
  - Automatically switches accounts without interruption when the active account reaches a usage limit.
  - Use `/accounts` to enable or disable accounts and their automation.
  - Can automatically start unused weekly usage windows and redeem banked usage resets shortly before expiry or when weekly usage is exhausted.
- Quality of life
  - Automatically waits and retries when a model is temporarily full.
  - Fixes TUI focus and input problems, plus crashes in long-running threads.
- Safer automation under `--yolo`
  - Hooks can send risky actions to Guardian for an automatic review instead of only allowing or blocking them.
  - Install and update Destructive Command Guard through `/codexplusplus`.

Codex++ otherwise stays close to upstream. It uses the normal `$CODEX_HOME` for configuration, sessions, plugins, and state, while imported account credentials live under `$CODEX_HOME/accounts`.

## Install a Release

Install Codex++ globally:

```sh
npm install -g --force @jjliebig/codex-plus-plus
```

The `--force` flag lets npm replace an existing upstream `codex` launcher without removing your Codex configuration or accounts. Codex++ npm releases support Windows x64, Linux x64 (musl), and macOS ARM64.

Run `codex update` for a normal Codex++ update. To switch a supported installation back to upstream Codex, run:

```sh
codex update upstream
```

To return to Codex++ later:

```sh
npm install -g --force @jjliebig/codex-plus-plus
```

Configuration and sessions continue to use `$CODEX_HOME`. Switching or reinstalling does not remove the Codex++ account store under `$CODEX_HOME/accounts`.

As a standalone alternative, download the matching package and `install-codex-plus-plus-latest` script from [Codex++ Releases](https://github.com/JJLiebig/codex/releases). The standalone installers verify release checksums and install an immutable package under `$CODEX_HOME/packages/codex-plus-plus`. Existing sessions keep their current release while new sessions use the update.

## Manage Accounts

```sh
codex account add
codex account list
```

Inside the TUI:

- `/accounts` configures accounts and per-account automation.
- `/codexplusplus` configures global fork features.

## Build Locally

Build and install a package using a temporary fork version such as `<upstream-version>-fork`:

```powershell
python scripts/build_codex_plus_plus.py --install
```

Without `--install`, the helper builds the reusable package directory at `dist/codex-plus-plus`. Explicit package arguments after `--` still override that default.

Local builds default to Cargo's `release-fast` profile for iteration. Public GitHub and npm binaries always use the full Cargo `release` profile.

## Publish a Release

Publishing is triggered only by manually pushing a tag named `codex-plus-plus-v<upstream>-fork.N` at the exact commit to release:

```sh
tag=codex-plus-plus-v0.146.0-fork.1 # use the current upstream version and next fork number
git tag "$tag"
git push origin "$tag"
```

The tag workflow validates the release helpers, builds Windows x64, macOS ARM64, and Linux musl x64 on standard hosted runners, verifies the exact npm payloads, publishes npm through OIDC trusted publishing with provenance, and publishes the GitHub Release only after npm succeeds. Configure the npm trusted publisher for `JJLiebig/codex`, workflow filename `codex-plus-plus-release.yml`, and the `npm publish` action; no npm token is used.

Release dependency caches are optional accelerators warmed on relevant `main` changes and restored by later tags. A missing, stale, corrupt, or unavailable cache produces the same cold build, and restored caches force target/profile-scoped v8 regeneration before the full `release` build. Intermediate artifacts expire after one day and are deleted after a successful release when GitHub permits it.

Reruns are safe after partial publication: exact npm versions are verified and skipped, conflicting registry content fails before any new version is published, and draft GitHub assets are replaced. Ordinary pull requests and `main` changes never publish.

## Remove the Shim

```powershell
.\scripts\install\install-codex-plus-plus.ps1 -Remove
```

```sh
sh scripts/install/install-codex-plus-plus.sh --remove
```

Removing or replacing the shim does not remove the account store under `$CODEX_HOME/accounts`.

Happy Codex-ing.
