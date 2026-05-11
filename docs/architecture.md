# Tartarus Architecture

This document describes the **structure** of the Tartarus codebase
and runtime: the components that exist, how they are arranged, how
they communicate, and where the boundaries are. It complements
[`spec.md`](./spec.md), which describes **what** Tartarus does and
**why**, and [`plan.md`](./plan.md), which describes **when** each
piece is built.

If the code disagrees with this document, one of them is wrong.

## High-level Topology

Tartarus has two pieces of code, on opposite sides of the VM
boundary:

```text
┌──────────────────────────────────────────────────────────────┐
│ HOST (invoking user, qemu:///session, no root)               │
│                                                              │
│  ┌──────────────┐   libvirt API    ┌────────────────────┐    │
│  │ tartarus CLI │ ───────────────▶ │ libvirtd (per-user)│    │
│  │  (Rust bin)  │  (virt crate)    └────────┬───────────┘    │
│  └──────┬───────┘                           │                │
│         │                                   ▼                │
│         │                            ┌────────────┐          │
│         │ qemu-guest-agent           │ QEMU/KVM   │          │
│         │  (virtio-serial,           │ (per-user) │          │
│         │   libvirt domain channel)  └─────┬──────┘          │
│         │                                  │                 │
│         │ stdio attach                     │                 │
│         ▼                                  ▼                 │
│ ┌────────────────────────────────────────────────────┐       │
│ │  Serial console PTY    (qemu-system serial port)   │       │
│ └────────────────────────────────────────────────────┘       │
└──────────────────────────────────────────────────────────────┘
                            │
                ╔═══════════╧═══════════╗
                ║  GUEST  (user mode VM)║
                ║                       ║
                ║  systemd → bootstrap  ║
                ║         → claude@user ║
                ║                       ║
                ║  in-guest helper:     ║
                ║    shell + units      ║
                ║                       ║
                ║  qemu-guest-agent     ║
                ║   (host→guest exec)   ║
                ╚═══════════════════════╝
```

The host side is a Rust binary. The guest side is a small bundle of
shell scripts and systemd units, baked into the base image. The
QEMU guest agent is the host→guest control channel; the serial
console is the user→guest interactive channel.

## Repository Layout

```text
tartarus/                          host-side Rust crate (binary + lib)
  Cargo.toml
  src/
    main.rs                        clap entry point, exit-code mapping
    lib.rs                         crate root; module declarations
    cli.rs                         clap subcommand definitions and dispatch
    config.rs                      TOML load/save, CLI/env merging, validation
    error.rs                       crate-wide `Error` enum (thiserror)
    logging.rs                     tracing-subscriber setup
    paths.rs                       XDG paths via `directories`
    host_user.rs                   in-guest user identity (uid/gid/username)
    time.rs                        calendar arithmetic (Howard Hinnant algorithm)
    doctor.rs                      diagnostic checks
    auth/                          credential acquisition + storage
      mod.rs
      error.rs                     `AuthError` variants
      google.rs                    `auth init google` interactive flow
      vertex.rs                    Vertex service-account JSON ingestion
      init.rs                      `auth init` interactive (GitHub + Anthropic)
      status.rs                    `auth status` (redacted)
      prompt.rs                    stdin/stdout prompt helpers
      redact.rs                    last-4-chars redaction
      write.rs                     atomic mode-0600 config writer
    host/                          libvirt + console + signals
      mod.rs
      error.rs                     `HostError` variants
      connect.rs                   `Connect` lifecycle, qemu:///session
      domain.rs                    domain XML templating + lifecycle
      console.rs                   PTY attach via virDomainOpenConsole
      agent.rs                     QEMU guest-agent client (guest-exec/file)
      signals.rs                   signal self-pipe (only `unsafe_code` carve-out)
    disk/                          qcow2 + base library
      mod.rs
      base.rs                      `tartarus base pull/list/prune`
      base/
        layering_seed.rs           layering NoCloud seed authoring
      overlay.rs                   per-session overlay creation
      grow.rs                      `tartarus grow` online resize
    gpu/                           opt-in PCI/VFIO GPU passthrough
      mod.rs                       `HostPreCheck` probe + gate
      error.rs                     `GpuError` variants
      pci.rs                       `PciAddress` + `PciDevice`, sysfs enumeration
      pci_ids.rs                   vendor/device name lookup from pci.ids
      iommu.rs                     IOMMU group inspection
      vfio.rs                      vfio-pci module detection
      driver.rs                    vendor-driver detach, vfio-pci bind, receipts
      quirks.rs                    vendor quirks (NVIDIA Code 43, AMD reset bug)
      setup.rs                     udev rule generation for user-mode VFIO
    seed/                          per-session NoCloud seed ISO
      mod.rs
      input.rs                     structured seed inputs (user/creds/repos/envs)
      render.rs                    cloud-init `user-data`/`meta-data` rendering
      iso.rs                       genisoimage shell-out
    session/                       per-session lifecycle
      mod.rs
      error.rs                     `SessionError` variants
      identity.rs                  UUID + alias symlink layout
      metadata.rs                  metadata.json schema + IO
      run.rs                       `tartarus run`
      run_mode.rs                  foreground / detached / background routing
      resume.rs                    `tartarus resume`
      stop.rs                      graceful + force shutdown
      destroy.rs                   undefine + cleanup
      list.rs                      `tartarus list` table
      rename.rs                    alias symlink rename
      env.rs                       `tartarus env add/update`
      update.rs                    `tartarus update` (running + stopped)
      ssh.rs                       per-session SSH keypair + known-hosts authoring
      ssh_attach.rs                `tartarus ssh` attach via OpenSSH
      ssh_port.rs                  loopback port allocation for SSH forwarding
      gpu_index.rs                 cross-session GPU borrow lookup + release
  tests/                           integration tests against real libvirtd
  examples/                        runnable end-to-end examples (Phase 11)

guest/                             in-guest helper (NOT a Rust crate)
  bin/
    tartarus-bootstrap.sh          first-boot: gh auth, repo clone(s), env writes
    tartarus-claude.sh             launches tmux on tty1, runs claude as user
    tartarus-env-wrapper.sh        sources /run/tartarus/env/* before exec
    tartarus-env-add.sh            idempotent install of rust|go|python
    tartarus-env-update.sh         idempotent update of installed envs
    tartarus-update.sh             dnf upgrade + claude updater + fstrim
    tartarus-update-claude.sh      Claude CLI updater (runs as user)
    tartarus-fstrim.sh             on-shutdown trim
    tartarus-grow.sh               periodic threshold watcher (drops marker)
    tartarus-grow-apply.sh         in-guest finisher invoked by `tartarus grow`
  systemd/
    tartarus-bootstrap.service     oneshot, after cloud-init
    tartarus-claude@.service       templated by username, runs on tty1
    tartarus-update-system.service oneshot, root, dnf upgrade
    tartarus-fstrim.service        ExecStart on shutdown target
    tartarus-grow.timer            periodic threshold check
    tartarus-grow.service          oneshot, root, runs the watcher

docs/
  spec.md                          MVP scope and behavior
  architecture.md                  this file
  plan.md                          phased build plan
  conventions.md                   coding style and rules
  quickstart.md                    auth init → base pull → run, one page
  acceptance.md                    live-acceptance checklist (Fedora workstation)
  auth.md                          credential walkthroughs + threat model

Cargo.toml                         workspace root
Cargo.lock                         committed
Makefile                           build/test/fmt/lint/audit/doc/help
clippy.toml, rustfmt.toml, deny.toml
CLAUDE.md, README.md, LICENSE
```

The single `tartarus` binary is the right choice for MVP. A future
split into `tartarus-core` (libvirt + disk + types) and
`tartarus-cli` (clap + subcommand glue) is a one-day refactor when
we need it.

## Configuration Subsystem

### Sources & Precedence

Three sources, in precedence order:

```text
CLI flags       >  env vars       >  config file       >  built-in defaults
(clap)             (clap env=)        (~/.config/tartarus/config.toml)
```

Every option that exists in any source exists in all three, wired
through `clap`'s `env =` attribute on a single argument declaration.
Adding an option means one diff, three sources covered.

### File

`~/.config/tartarus/config.toml`, mode `0600`, located via the
`directories` crate (`ProjectDirs::from("", "", "tartarus")`).
Schema covers credentials, Claude defaults, base envs, Rust
component/toolchain choices, the user identity (overridable; defaults
to invoker), repository defaults, network defaults, disk defaults.

The full schema lives in `spec.md` and is the single source of truth.
The `config` module's job is to deserialize, validate, and merge — not
to enumerate options.

### Validation

Validation happens at load time, not at use time. A bad config fails
the CLI at startup with a clear error. Refusing to run as root is one
such validation; missing GitHub credentials at `run` time is another.

## Authentication Subsystem

The `auth` module owns all credential acquisition, storage, and
serialisation into cloud-init payloads. It is the only place that
talks to GitHub's auth endpoints, handles GCP service-account
files, or writes credentials to disk.

### Credentials Supported (MVP)

- **GitHub PAT**: paste-direct only. (The OAuth device-flow
  fallback is deferred to a future milestone.)
- **Anthropic API key**: paste-direct or "open browser, paste back."
- **Vertex AI**: service-account JSON file path. `tartarus auth
  init google` is the dedicated subcommand for bootstrapping the
  Vertex backend (collects project ID, region, file path).

Credentials live in `~/.config/tartarus/config.toml` (mode 0600). At
session-start time they are read, copied into the cloud-init seed
ISO as either env vars or `write_files` entries, and never leave
that channel.

### GitHub Device Flow

Hand-rolled HTTP against `https://github.com/login/device/code` and
`/login/oauth/access_token`. ~50 lines, no `oauth2` crate
dependency.

### Vertex Mechanics

The service-account JSON file is read on the host, written to the
guest at `/etc/tartarus/google-credentials.json` (mode 0600, owned
by the in-guest user) via cloud-init `write_files`. Four env files
are dropped into `/run/tartarus/env/` for the Vertex backend:

- `GOOGLE_APPLICATION_CREDENTIALS=/etc/tartarus/google-credentials.json`
- `CLAUDE_CODE_USE_VERTEX=1`
- `CLOUD_ML_REGION=<region>` (the GCP region from `[claude.vertex] region`)
- `ANTHROPIC_VERTEX_PROJECT_ID=<project_id>`

`tartarus-env-wrapper.sh` sources every file under
`/run/tartarus/env/` before exec'ing `claude`, so each invocation
sees the four variables plus `GITHUB_TOKEN`, `CLAUDE_MODEL`, and
`CLAUDE_EFFORT`. The Anthropic backend is mutually exclusive: when
it is selected, `ANTHROPIC_API_KEY` is written and none of the
Vertex env files (or the SA JSON file) are. `bedrock` is recognised
at the parser level so the config-load step can reject it with a
milestone-2 hint instead of a generic "unknown variant" error.

## Host Subsystem

### libvirt Connection

A single `host::connect::Connect` per CLI invocation, lazy. The MVP
talks only to `qemu:///session`; the URI is read from config (default)
or `--uri` (override). All libvirt operations go through this
connection; no parallel libvirtd connections, no cached connections
across invocations (a CLI run is short-lived).

### Domain XML

`host::domain` templates the libvirt domain XML for each session
from a struct: UUID-as-name, the per-session overlay disk, the
cloud-init seed ISO as a virtual CD-ROM, a serial console PTY, a
virtio-serial channel for `qemu-guest-agent`, SLIRP networking, and
either a virtio-rng or `<rng>` device for entropy. Generated XML is
written to the session directory as `domain.xml` for inspection and
debugging.

### Guest Agent Channel

`qemu-guest-agent` runs in the guest as a systemd service, baked into
the Tartarus layer. The host talks to it through a `<channel
type='unix'>` device wired to libvirt's named-channel mechanism. The
`host::agent` module wraps `virDomainAgentSetResponse` /
`virDomainAgentCommand` (or the higher-level helpers in the `virt`
crate) into a small typed API: `exec`, `exec_status`, `file_write`,
`file_read`, `ping`. This is the host→guest control channel for
updates, env operations, and grow signals.

### Console Attach

`host::console` wraps `virDomainOpenConsole` to obtain a libvirt
stream, sets the host TTY into raw mode, and pumps bytes both
directions. The Ctrl-A D escape sequence is detected on the input
side and triggers a clean detach (TTY restored, stream closed,
domain left running). SIGINT, SIGTERM, and SIGHUP detach the same
way: `host::signals` installs an async-signal-safe handler that
writes one byte into a self-pipe; a watcher thread reads that pipe
and posts a `DetachReason::Signal` on the same `mpsc` the pumps
use, so the `RawModeGuard` Drop always runs and the host TTY is
always restored. `host::signals` is the workspace's only
`#![allow(unsafe_code)]` carve-out — Rust's stdlib does not expose
signal-handler installation without either `unsafe` or a third-party
crate, and the project's dependency rule rules the latter out.
There is no inline shutdown shortcut — shutdown is `tartarus stop`
from another shell.

### Run Modes

`session::run` supports three start-time modes (see `spec.md` for
the user-facing description):

- **Foreground** (default): start the domain, attach the console,
  block until the user detaches or the guest shuts down.
- **Detached** (`--detach`): start the domain, do not attach the
  console, return immediately. `session::resume` is the
  re-attach path.
- **Background** (`--background`): same lifecycle as detached,
  plus the seed builder injects `claude`'s remote-connectivity
  flags (the env-var contract is fixed in `auth::claude_remote`)
  so the session is drivable from claude.ai web/mobile. The
  remote-connect URL/token is captured from Claude's first-boot
  output via `qemu-guest-agent` and printed to stdout *and*
  recorded in `metadata.json`.

`--detach` and `--background` are mutually exclusive at the clap
level; the `RunMode` enum is the single source of truth and flows
through `session::run` into `seed::Seed` (which decides what env
to inject) and `host::domain` (which decides whether to attach).

## Disk Subsystem

### Base Library

`disk::base` owns `~/.local/share/tartarus/base/`. Layout is
versioned files plus a `current` symlink:

```text
base/
  current -> fedora-41-2026-05-01.qcow2
  fedora-41-2026-05-01.qcow2
  fedora-41-2026-04-01.qcow2
```

`tartarus base pull` flow:

1. Fetch the cloud image from `download.fedoraproject.org` over
   TLS (full chain + hostname verification, no fallback). HTTP
   client is `reqwest` with rustls.
2. Fetch the matching `RPM-GPG-KEY` over TLS if not already
   trusted.
3. Verify the image's detached signature by shelling to `gpgv`
   with the trusted key as the keyring.
4. Apply the **Tartarus layer** by booting the verified image with
   a layering cloud-init seed ISO and waiting for clean shutdown.
   The layering seed installs packages, configures envs, installs
   Claude under `~/.local`, drops the in-guest helper scripts and
   units, and powers off. The result is a sealed, repeatable base.
5. Move the result to its versioned filename and update `current`.

Layering by booting the image is slower than `virt-customize` but
adds no system dependency beyond what we already require. It runs
once per Fedora release.

`tartarus base prune` reads each session overlay's backing-file
pointer (`qemu-img info`, parsed) and deletes any base image with no
remaining overlay references. `current` is preserved unconditionally.

`tartarus base build` (custom recipes) is **deferred to milestone 2+**;
not in MVP.

### Per-Session Overlays

`disk::overlay` creates and destroys per-session qcow2 overlays:

```text
qemu-img create -f qcow2 \
  -b ~/.local/share/tartarus/base/current \
  -F qcow2 \
  ~/.local/share/tartarus/sessions/by-uuid/<uuid>/overlay.qcow2 \
  100G
```

Default virtual size is 100 GiB (config-driven). The overlay is
sparse; on-disk size grows only with allocated clusters. Discard is
enabled (`discard=unmap`) so `fstrim` from the guest reflects back
into the overlay file.

### Auto-Grow

`disk::grow` coordinates the online-resize sequence. The MVP path is
explicit: the user runs `tartarus grow <alias|uuid>` (or P11+ a
host-side cron) when the in-guest watcher's marker file appears.

1. Guest watcher (`tartarus-grow.timer` + `.service`) writes
   `/run/tartarus/grow-request` when `df --output=pcent /` crosses
   `[disk] grow_threshold_pct`.
2. `tartarus grow` resolves the session, opens libvirt, confirms the
   domain is running, and reads the overlay's current virtual size
   via `qemu-img info --output=json`.
3. Host runs `qemu-img resize <overlay>.qcow2 +<increment>G`.
4. Host calls `virDomainBlockResize` (with `VIR_DOMAIN_BLOCK_RESIZE_BYTES`)
   to notify the running guest.
5. Host dispatches `tartarus-grow-apply.sh` over qemu-guest-agent;
   the script runs `growpart` and dispatches the matching
   filesystem-grow verb (`resize2fs` / `xfs_growfs` /
   `btrfs filesystem resize max`) on whatever `findmnt -no FSTYPE /`
   reports.
6. Host writes the new size into `metadata.json`
   (`overlay_virtual_gib`); `tartarus list` surfaces it as a SIZE
   column.

All sizes and thresholds are config-driven; the user rarely touches
them. Online grow requires the session to be running; offline grow
is not part of MVP. The first-boot path is handled separately by
cloud-init's `growpart` module + `resize_rootfs: true` directive in
the per-session seed (see [`crate::seed::render`]).

## Seed Subsystem

`seed` builds the cloud-init NoCloud seed ISO that bootstraps each
session.

### Inputs

A struct describing what the session needs at first boot:

- The user (username, UID, GID).
- All credentials selected for this run (GitHub PAT, Anthropic key
  or Vertex bundle, etc.).
- The list of repos to clone, with the default repo identified.
  CLI `--repo` (repeatable) is the canonical source; `[base] repos`
  in config is the fallback when no `--repo` is passed. The default
  repo is selected via, in order: `--default-repo`, `[base] default_repo`,
  the `default = true`-flagged config entry, the first listed slug.
  At most one config entry may be flagged default; more than one is
  rejected at config-load time.
- The selected programming envs to activate.
- Claude defaults (model, effort, etc.).

### Output

A small ISO 9660 image at `<session-dir>/cloud-init.iso` containing
two files:

- `user-data` — cloud-init script with `write_files`, `runcmd`,
  `users`, `package_update`, etc.
- `meta-data` — the instance-id and local-hostname.

Generated by shelling to `genisoimage` (or `xorrisofs`):

```console
genisoimage -output cloud-init.iso -volid cidata -joliet -rock \
            user-data meta-data
```

`genisoimage` is a system dependency. The intent of the no-shell rule
is libvirt operations specifically; ISO authoring is a different
domain and shelling is appropriate. If we later want to drop the
system tool, the `fatfs` crate provides a pure-Rust path (NoCloud
also accepts vfat-formatted seeds with the `cidata` label).

## Session Subsystem

### Identity & Layout

Per-session directory under `~/.local/share/tartarus/sessions/by-uuid/<uuid>/`:

```text
overlay.qcow2          ephemeral or persistent disk
cloud-init.iso         seed (regenerated on each run, e.g. for cred rotation)
domain.xml             libvirt definition (audit + debug aid)
metadata.json          alias, repos, base, envs, persist flag, timestamps
```

Aliases live in `sessions/by-name/<alias>` as relative symlinks
into `by-uuid/`. Aliases are pure host-side metadata; libvirt only
sees the UUID as the domain name.

### Lifecycle Operations

All session operations route through `session::*`:

- `run` creates UUID + dir, writes overlay, builds seed, defines and
  starts domain, optionally attaches console.
- `resume` looks up by alias or UUID, starts if shutoff, attaches
  console.
- `stop` graceful shutdown (libvirt `shutdown`), with a timeout
  before falling back to `destroy` (forced).
- `destroy` deletes overlay (if not `--keep-overlay`), undefines the
  libvirt domain, removes the session dir and any aliases.
- `list` reads all session dirs, queries libvirt for live status,
  prints alias / UUID / status / base / envs / persist.
- `rename` creates or moves the alias symlink.

### Metadata

`metadata.json` is the host-side ground truth for a session. Schema
(versioned via a `version: 1` field):

```json
{
  "version": 1,
  "uuid": "...",
  "alias": "fix-session-cleanup",
  "base": "fedora-41-2026-05-01",
  "repos": [{"slug": "owner/name", "default": true}],
  "envs": ["rust", "go", "python"],
  "persist": true,
  "created_at": "...",
  "last_attached_at": "..."
}
```

Schema bumps follow a "load any version, write current" pattern:
the `metadata` module knows how to read v1 and any future versions,
but always writes current.

## Update Subsystem

`update` runs against either a running or stopped session:

- **Stopped**: boots the domain in update mode (no console attach,
  marked via cloud-init env), runs the update steps, shuts down.
- **Running**: dispatches commands through `qemu-guest-agent`'s
  `guest-exec` to the in-guest helper. User-visible processes are
  not disturbed.

Both paths run the same three steps:

1. `dnf upgrade --refresh -y` — root via the
   `tartarus-update-system.service` systemd unit.
2. Claude CLI update — runs as the user against the per-user
   `~/.local` install. Never as root, never via system `npm`.
3. `fstrim -av` — root.

`tartarus env update` is the env-specific path: `rustup update`,
`cargo install --locked` for the configured cargo tools, `dnf
upgrade golang`, `dnf upgrade python3 python3-virtualenv`. All
operations are idempotent.

## Env Subsystem

`env::add` installs a programming environment into a session.
Idempotent: a no-op when the env is already present.

`env::update` brings installed envs current. Idempotent.

Both delegate execution to the in-guest helper via `qemu-guest-agent`.

## GPU Passthrough Subsystem

The `gpu` module owns opt-in PCI passthrough for sessions that
need real GPU acceleration (CUDA/ROCm workloads). It is the only
feature that materially changes Tartarus' threat model: a session
with a borrowed device shares more host state than a stock session
(kernel driver detach, IOMMU group co-residency). All sysfs writes
and driver operations are isolated inside `crate::gpu`.

Surface:

- `PciAddress` / `PciDevice` — typed PCI identifiers with sysfs
  enumeration and vendor/device name lookup from `pci.ids`.
- `IommuGroup` — one IOMMU group's identifier and member list,
  with a cleanliness check for passthrough safety.
- `HostPreCheck` — outcome of the host-side gate (IOMMU enabled,
  `vfio-pci` loaded, target group clean).
- `driver::borrow` / `driver::release_with_receipt` — detach the
  vendor driver, bind `vfio-pci`, and the inverse. The receipt
  (previous driver, device address) is persisted in `metadata.json`
  so a crashed session can be released with `tartarus host gpu
  release <bdf>`.
- `quirks` — vendor-specific workarounds (NVIDIA Code 43 hide-KVM,
  AMD reset-bug refusal).
- `setup::build_udev_rule` — renders the udev rule that grants the
  invoking user read+write on `/dev/vfio/<group>` so
  `qemu:///session` can use VFIO without `--privileged-libvirt`.

CLI surface: `tartarus host gpu list`, `tartarus host gpu status
[--bdf BDF]`, `tartarus host gpu setup-gpu`, `tartarus host gpu
release <bdf>`. The `--gpu auto|<bdf>` flag on `tartarus run`
triggers the borrow at session start; `--privileged-libvirt`
switches to `qemu:///system` when the VFIO group nodes are not
user-readable.

## SSH Attach Subsystem

`tartarus ssh <alias|uuid>` attaches to a running session over
SSH instead of the serial console. The session module owns three
files for this:

- `ssh.rs` — per-session ed25519 keypair generation (via
  `ssh-keygen`) and host-key capture from the guest via
  `qemu-guest-agent`. Keys are stored under the session's
  `ssh/` directory at mode 0600/0700.
- `ssh_attach.rs` — orchestrates the SSH invocation with
  `StrictHostKeyChecking=yes`, `IdentitiesOnly=yes`, and a
  per-session `UserKnownHostsFile`. No global
  `~/.ssh/known_hosts` pollution, no TOFU from the network.
- `ssh_port.rs` — loopback port allocation for QEMU's SLIRP
  host-forward. The port is probed then immediately released
  (acknowledged TOCTOU; mitigated by metadata-side dedup).

The public key is injected into cloud-init's
`ssh_authorized_keys` at seed-build time. The host key is read
from the guest after boot so the first SSH connection verifies
against a known fingerprint rather than trusting the network.

## Doctor Subsystem

`tartarus doctor` runs a series of diagnostic checks and prints
their status:

- libvirtd reachable on `qemu:///session`?
- `/dev/kvm` accessible to the invoking user?
- A base image is present and `current` resolves?
- The base's GPG key is trusted?
- Egress to `download.fedoraproject.org` works (TLS strict)?
- `genisoimage` and `gpgv` are on `PATH`?
- The XDG paths exist and are writable?
- Any orphaned domains (libvirt sees a `tartarus-` UUID with no
  on-disk session dir)?

Each check is independent; failures are reported with a remediation
hint. Exit code is the count of failures (0 on success).

## In-Guest Helper

The in-guest side is shell + systemd, baked into the base image.
Bash, no Python, no Rust. The contract surface is:

- `tartarus-bootstrap.service` — runs once per first boot; reads
  the cloud-init payload, runs `gh auth login`, clones each repo
  into `$HOME/tartarus/repositories/<repo>` at depth 1, writes env
  files into `/run/tartarus/env/`, starts
  `tartarus-claude@<user>.service`.
- `tartarus-claude@<user>.service` — runs on `tty1` as the named
  user, launches a tmux session named `work` cd'd to the default
  repo, then runs `claude` inside it (with `tartarus-env-wrapper.sh`
  sourcing env files first).
- `tartarus-fstrim.service` — `ExecStart=/usr/local/bin/tartarus-fstrim.sh`
  wired into `shutdown.target`.
- `tartarus-grow.timer` / `.service` — periodic threshold check;
  signals the host via `qemu-guest-agent` when the watermark is
  crossed.

Helper sources are tracked under `guest/` in the repository and
copied into the base image during the layering step. Updates to the
helper require rebuilding the base (or pushing the new bits via
`tartarus update` for running sessions; the update path includes a
helper-refresh hook).

## Logging

The `logging` module configures `tracing-subscriber` once at startup
based on:

- `--quiet`, `--verbose`/`-v`, `-vv` flags (CLI > env > config).
- `TARTARUS_LOG` env var (filter directive, e.g.
  `tartarus=debug,virt=warn`).
- `--log-format json` for machine-readable output.

Default destination is stderr. There is no host-side log file in
MVP; if the user wants persistent logs, redirection is their job.

## Errors

A single crate-wide `tartarus::error::Error` enum, derived via
`thiserror`. Sub-modules may define their own narrow error types
that flatten into the root via `From` impls. The public CLI
result type is `Result<T, Error>`. There is no `anyhow`.

User-facing errors (the strings the CLI actually prints) are
formatted with context and a remediation hint where possible. The
underlying chain is preserved for `--verbose` output.

## Networking

The host side makes outbound HTTPS to:

- `https://download.fedoraproject.org/...` (image artifacts)
- `https://getfedora.org/...` (key fetch fallback, if needed)

The OAuth device-flow endpoints
(`https://github.com/login/device/code`,
`https://github.com/login/oauth/access_token`) are not currently
contacted; the MVP `auth init` flow is paste-only. The device-flow
fallback is deferred to a future milestone.

All TLS is strict: full chain validation, hostname verification, no
fallback. `reqwest` configured with `rustls-tls`, no native-tls
fallback. There is no insecure-skip-verify option exposed.

The guest side, in MVP, has unrestricted outbound through SLIRP NAT.
No inbound from the host.

## Testing

Three layers:

- **Unit tests** under `#[cfg(test)] mod tests` blocks, per
  conventions. No real libvirtd touched.
- **Integration tests** under `tartarus/tests/`, against a real
  `qemu:///session` libvirtd. Each file corresponds to the phase
  that introduced the capability (e.g. `host_phase3.rs`,
  `base_phase4.rs`). CI runs these on a usermode KVM runner; no
  `qemu:///system` tests in MVP.
- **Worked examples** under `tartarus/examples/`, each runnable via
  `cargo run --example <name>` against a real libvirtd. Per
  conventions, each new capability ships with one.

The end-to-end "the example workflow in `spec.md` runs to
completion" check is the MVP acceptance gate.

## Platform Support

Tartarus supports two host operating systems with different roles:

- **Linux**: full support. Runs Tartarus locally and drives VMs on
  the same machine via `qemu:///session`. Also drives VMs on
  remote Linux hosts via `qemu+ssh://`. **The MVP supports Linux
  only.**
- **macOS**: client-only, **arrives with milestone 2**. Tartarus
  runs on the Mac, opens an SSH transport to a remote Linux host,
  and drives that host's per-user libvirtd
  (`qemu+ssh://user@host/session`). macOS never runs `libvirtd`,
  QEMU, or KVM itself; it links against Homebrew's `libvirt`
  client headers. The seed ISO is authored on the host that has
  `genisoimage`; for a Mac client this means authoring on the
  remote Linux host (a milestone-2 detail).
- **Windows**: not supported.

In all cases Tartarus drives **user-mode libvirt only** and
refuses to start as `euid 0`.

## Distribution

`cargo install tartarus` (eventually published to crates.io) is the
MVP distribution mechanism. The host requires `libvirt-dev` /
`libvirt-devel` headers at build time and `libvirtd` running on the
session bus at run time, plus `genisoimage` and `gpgv`. The full
runtime requirement set lives in `README.md`.
