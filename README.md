# Tartarus

A security sandbox for running AI coding agents inside disposable
virtual machines.

## What it is

Tartarus runs untrusted AI agent code in throwaway QEMU/KVM virtual
machines on local or remote Linux hosts.

The agent gets a real Linux environment to operate in: compilers,
package managers, network access under your control. The host is
isolated from anything the agent does inside the VM.

Tartarus is written in Rust and drives QEMU/KVM through [libvirt] via
via the upstream [virt] crate.

[libvirt]: https://libvirt.org/
[virt]: https://crates.io/crates/virt

## Requirements

- A Linux host (local or remote) with KVM enabled
  (`/dev/kvm` readable by the invoking user; membership in the
  `kvm` group is the canonical fix when access is denied).
- `libvirtd` running under the **invoking user's session bus** on
  every host Tartarus will manage. Confirm with
  `systemctl --user status libvirtd`; enable with
  `systemctl --user enable --now libvirtd`. The system libvirtd is
  not used.
- `libvirt-dev` (Debian/Ubuntu) or `libvirt-devel` (Fedora/RHEL)
  headers at build time on Linux; `brew install libvirt` on macOS.
- `passt` on `PATH` for user-mode networking with port
  forwarding. Install: `dnf install passt` (Fedora/RHEL)
  or `apt install passt` (Debian/Ubuntu).
- `qemu-utils` (Debian/Ubuntu) or `qemu-img` (Fedora/RHEL) on
  `PATH` for overlay management.
- `genisoimage` (or `xorrisofs`) on `PATH` for cloud-init seed
  authoring.
- `gpgv` on `PATH` for verifying the Fedora release signature.
- The base image installs `cloud-utils-growpart` *inside the guest*;
  no host-side action is needed for the auto-grow path.

Build from a clone (until published):

```console
cargo install --path tartarus
```

After publication this will be `cargo install tartarus`.

## Documentation

- [Quickstart](./docs/quickstart.md) — `auth init` → `base pull` → `run` in one page.
- [Auth setup](./docs/auth.md) — full walkthroughs (GitHub PAT,
  Anthropic, Vertex AI service account), threat model, rotation.
- [Specification](./docs/spec.md) — MVP scope, architecture, lifecycle, CLI
- [Architecture](./docs/architecture.md) — code structure, module layout, runtime topology
- [Plan](./docs/plan.md) — phased build plan and acceptance criteria
- [Acceptance](./docs/acceptance.md) — live-fire checklist for a real Fedora workstation
- [Conventions](./docs/conventions.md) — coding style, testing, lints

## License

See [`LICENSE`](./LICENSE).
