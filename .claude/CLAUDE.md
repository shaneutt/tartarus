# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when
working with code in this repository.

## What this project is

Tartarus is a security sandbox for running AI coding agents inside
disposable QEMU/KVM virtual machines, on local or remote Linux hosts.
See [`README.md`](./README.md) for the user-facing summary.

## Quick Reference

```console
make build          # cargo build --workspace
make test           # cargo test --workspace
make fmt            # format with nightly rustfmt
make lint           # clippy -D warnings + nightly rustfmt --check
make audit          # cargo audit + cargo deny check
make doc            # rustdoc with -D warnings
```

Run a single test:

```console
cargo test <test_name>
cargo test <test_name> -- --nocapture   # with output
```

Run tests with output globally:

```console
make test V=1
```

`make fmt` and `make lint`'s rustfmt step require nightly toolchain
(stable rustfmt does not support the options in `rustfmt.toml`).
`make audit` requires `cargo install cargo-audit cargo-deny`.

## Foundation

- Language: **Rust**, edition 2024, MSRV 1.88
- Virtualization: **QEMU/KVM**, managed through **libvirt**
- libvirt is accessed via the **`virt` crate** (the official upstream
  binding, `gitlab.com/libvirt/libvirt-rust`, published by the libvirt
  project itself). Local + remote hosts are both reached through
  libvirt's native transports (`qemu:///system`, `qemu+ssh://`,
  `qemu+tls://`); we do not write our own remote-execution layer.
- System requirements: `libvirt-dev` / `libvirt-devel` headers at
  build time; a running `libvirtd` on every host (local or remote)
  at runtime.

## Architecture

The codebase has two sides separated by the VM boundary:

**Host side** (`tartarus/src/`): a single Rust crate (binary + lib)
that drives libvirt, manages disk images, and orchestrates sessions.
Key subsystems:

- `cli`: clap surface and dispatch. The only place `println!` is
  allowed.
- `config`: TOML file + CLI flags + env vars, merged in precedence
  order (CLI > env > file > built-in defaults). Validation at load
  time, not use time.
- `auth`: credential bootstrap, redacted status, atomic config
  writes. Supports GitHub PAT, Anthropic API key, and Vertex AI
  service-account JSON.
- `host`: libvirt connection (`connect`), domain XML templating +
  lifecycle (`domain`), serial console PTY attach (`console`),
  QEMU guest-agent client (`agent`), signal self-pipe (`signals`).
- `disk`: base-image library with GPG verification (`base`),
  per-session qcow2 overlays (`overlay`), online resize
  coordination (`grow`).
- `seed`: cloud-init NoCloud seed ISO authoring (structured inputs,
  user-data/meta-data rendering, genisoimage shell-out).
- `session`: per-session lifecycle (run, resume, stop, destroy,
  rename, list, update, env, ssh attach).
- `gpu`: PCI device discovery, IOMMU group inspection, VFIO
  driver bind/unbind, udev rule generation for GPU passthrough.
- `error`: single crate-wide `Error` enum via `thiserror`. No
  `anyhow`.

**Guest side** (`guest/`): shell scripts + systemd units baked into
the base image. Not a Rust crate. The `qemu-guest-agent` is the
host-to-guest control channel; the serial console is the
user-to-guest interactive channel.

### Data flow for `tartarus run`

1. Config loaded and validated (`config`)
2. Session UUID + directory created (`session::identity`)
3. qcow2 overlay created from base image (`disk::overlay`)
4. Cloud-init seed ISO built from credentials + repos + envs
   (`seed`)
5. Domain XML templated and defined via libvirt (`host::domain`)
6. Domain started; console attached or returned depending on run
   mode (`session::run_mode`)

### Session storage layout

```text
~/.local/share/tartarus/
  base/
    current -> fedora-41-YYYY-MM-DD.qcow2
    fedora-41-YYYY-MM-DD.qcow2
  sessions/
    by-uuid/<uuid>/
      overlay.qcow2
      cloud-init.iso
      domain.xml
      metadata.json
    by-name/<alias> -> ../by-uuid/<uuid>/
```

## Hard rules

### Dependencies

**Do NOT add, upgrade, or replace any dependency without explicit
permission from the maintainer.** This applies to:

- Any new entry in any `Cargo.toml` (`[dependencies]`,
  `[dev-dependencies]`, `[build-dependencies]`, workspace
  dependencies, optional features that pull new crates).
- Version bumps of existing dependencies, including patch bumps.
- New transitive dependencies introduced by enabling cargo features.
- New system packages or build-time tools.

If a task appears to require a new dependency, **stop and ask first**.
Propose the crate, explain why it is needed, and what the alternative
is (writing it ourselves, using `std`, using something already in the
tree). Wait for an explicit "yes" before editing `Cargo.toml` or
`Cargo.lock`.

The only pre-approved dependency is the `virt` crate (and its
`virt-sys` companion, pulled in transitively).

### Scope

- No speculative abstractions, no "future-proofing," no parallel
  implementations. Build what the current task requires.
- No CLI shelling out to `virsh` / `qemu-system-*` when a libvirt API
  call exists. The whole point of the `virt` crate is to avoid that.

### Workflow

All work lands directly on `main`. No feature branches, no
`claude/...` per-session dev branches, no git worktrees. If the
harness defaults to a per-session dev branch, override it: commit
and push to `main` directly.

## Conventions

See [`docs/conventions.md`](./docs/conventions.md) for the full
coding style guide. Key points:

- `#![deny(unsafe_code)]` in all crates. Sole carve-out:
  `tartarus/src/host/signals.rs`, where
  `#![allow(unsafe_code)]` lets us install POSIX signal
  handlers via raw `extern "C"` declarations (see the
  module doc comment for rationale).
- All items (public and private) require `///` doc comments;
  enforced by the `missing_docs` lint.
- Comments answer "why?", never "what?"; use `tracing` for runtime
  narration.
- Errors via `thiserror`; logging via `tracing`.
- Prefer `to_owned()` over `to_string()` for `&str` to `String`.
- Use inline format args: `format!("{var}")`.
- Reference-style rustdoc links, not inline.
- Do not document memory efficiency in rustdoc (e.g. "avoids
  allocation", "zero-copy", "cheap clone").
- No re-export-only files. Import directly from the source module.

## File Ordering and Separator Comments

Use full-width separator comments to delineate logical sections.
Section names should be **semantic** (describe the contents),
not visibility-based. For example: `// HostUser`, `// Validation`,
`// Utility Functions`, `// Tests`, not `// Public API` or
`// Private Utilities`.

```rust
// -----------------------------------------------------------------------------
// Section Name
// -----------------------------------------------------------------------------
```

General ordering within a file:

1. Constants
2. Primary types, impls, and functions
3. Supporting types and impls
4. Utility functions
5. `#[cfg(test)] mod tests` (always last)

Inside `mod tests`: imports, test functions, then test utilities
(with `// Test Utilities` separator).

Struct fields: `name` first (if present), then alphabetical. Impl
blocks: `new()` first, then `name()`, then alphabetical.

Exception: `clap`-derive argument structs follow CLI-affordance
order (positional/required first, flags grouped, subcommand last).

## Test Requirements

New capabilities require:

1. Unit tests.
2. Integration tests against a real `libvirtd`.
3. A worked example under `examples/`.

See [`docs/conventions.md`](./docs/conventions.md) for full test
conventions (no inline comments in test bodies, no doc comments on
test functions, full-width separators only).

## Code Responsibility

Every contributor is responsible for the code they submit,
regardless of how it was produced. All code MUST be human-reviewed
before merging. Signed-off commits (`Signed-off-by:`) are required.
PRs from bots or tools (excluding `dependabot`-class ones) will not
be accepted. See [`docs/conventions.md`](./docs/conventions.md#code-responsibility)
for the full policy.
