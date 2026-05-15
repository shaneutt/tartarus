# Tartarus Quickstart

This walkthrough takes a fresh Linux workstation to a running Claude
session inside a disposable VM. It covers the happy path only; for
deeper material see [`spec.md`](./spec.md), [`architecture.md`](./architecture.md),
and [`README.md`](../README.md).

## 1. System prerequisites

Tartarus needs a Linux host with KVM available, libvirt running on the
**user session bus**, and a few CLI tools on `PATH`.

Install on Fedora:

```console
sudo dnf install libvirt-daemon-config-network libvirt-daemon-driver-qemu \
                 libvirt-daemon-kvm libvirt-devel \
                 passt qemu-img genisoimage gnupg2
sudo usermod -aG kvm $USER
# log out / back in for the kvm group to take effect
systemctl --user enable --now libvirtd
```

On Debian / Ubuntu (`apt`), the analogous packages are
`libvirt-daemon-system libvirt-clients libvirt-dev passt
qemu-utils genisoimage gnupg`. Membership in the `kvm` group and a running
user-session libvirtd are the same.

Confirm `qemu:///session` is reachable, `/dev/kvm` is readable, and the
required tools are on `PATH`:

```console
tartarus doctor
```

A clean run reports `All checks passed.` Anything else carries a
remediation hint inline.

## 2. Install Tartarus

The crate is not yet on `crates.io`, so install from a clone:

```console
git clone https://github.com/the-lost-art-of-programming/tartarus
cd tartarus
cargo install --path tartarus
```

Once published, this becomes `cargo install tartarus`.

## 3. Bootstrap credentials

Tartarus needs a GitHub PAT (to clone repos and authenticate `gh`
inside the guest) and one Claude backend credential (Anthropic API
key, or a Vertex service-account file). [`docs/auth.md`](./auth.md)
is the full reference; the short form:

```console
tartarus auth init
```

GitHub: paste a personal access token created at
<https://github.com/settings/tokens> with the `repo` scope. Anthropic:
paste an API key from <https://console.anthropic.com/settings/keys>,
or hit Enter to open the console and paste back. The credentials land
in `~/.config/tartarus/config.toml` at mode `0600`.

For the Vertex (Google Cloud) backend instead of Anthropic:

```console
tartarus auth init google
```

This collects the GCP project ID, region (default `us-east5`), and the
path to the service-account JSON file. The full step-by-step including
the `gcloud` commands to create the service account and the IAM role
to grant lives in [`docs/auth.md`](./auth.md).

## 4. Pull the base image

```console
tartarus base pull
```

This downloads the latest Fedora cloud base over strict TLS, GPG-verifies
it against Fedora's release key, then boots the image once with a
layering cloud-init seed that installs every package the in-guest
helper needs (Claude, `gh`, the programming-environment toolchains,
`qemu-guest-agent`, etc.) and powers off cleanly. The result is the
sealed `base/current` image every subsequent session boots from.

The first pull takes 5–10 minutes on a residential connection (most of
which is the layering boot). Subsequent pulls only re-layer when Fedora
ships a new cloud image.

## 5. Run a session

```console
tartarus run --repo owner/name
```

The session boots, cloud-init clones the repo into
`~/tartarus/repositories/<repo>`, and `claude agents` starts
inside the VM. The host connects to the guest via SSH on an
automatically allocated loopback port.

To re-attach later:

```console
tartarus connect <alias-or-uuid>
```

To list everything that exists:

```console
tartarus list
```

To shut a session down cleanly:

```console
tartarus stop <alias-or-uuid>
```

To delete it (overlay, alias, libvirt domain):

```console
tartarus destroy <alias-or-uuid>
```

## Where to next

- [`spec.md`](./spec.md) — full MVP scope, configuration schema, and CLI surface.
- [`architecture.md`](./architecture.md) — how the pieces fit together.
- [`tartarus/examples/`](../tartarus/examples) — runnable single-purpose examples for each major capability.
