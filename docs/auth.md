# Tartarus authentication setup

`tartarus run` needs three things to drive a useful agent session:

1. A **GitHub personal access token (PAT)** so the in-guest user can
   `git clone` your repos and authenticate `gh`.
2. A **Claude backend credential** — either an Anthropic API key *or*
   a Google Cloud Vertex AI service-account file.
3. (Optional) A configuration override for the in-guest user
   identity, programming environments, etc. — covered in
   [`spec.md`](./spec.md), not here.

Everything below is one-time per workstation. The credentials live
at `~/.config/tartarus/config.toml`, mode `0600`, and only the
invoking user can read them. The seed ISO that carries them into
each session lives at `~/.local/share/tartarus/sessions/by-uuid/<uuid>/`,
also mode `0600` per file in a `0700` directory.

> **Where to find the prompts:** The walkthroughs below match the
> exact prompts `tartarus auth init` and `tartarus auth init google`
> emit. Empty input at a GitHub prompt is rejected; empty input at
> an Anthropic prompt opens the console in your browser.

## TL;DR

```console
# Anthropic backend (most common):
tartarus auth init

# Vertex AI backend instead:
tartarus auth init           # supplies GitHub + a stand-in Anthropic key
tartarus auth init google    # adds the Vertex bundle and switches the backend

# Verify and inspect:
tartarus auth status
tartarus doctor
```

If your config is already populated and you only want to swap
backends, `tartarus auth init google` merges into the existing file
without overwriting your GitHub token.

---

## 1. GitHub personal access token

Tartarus is paste-only for GitHub. The OAuth device-flow fallback
the original plan called for is deferred until the project
registers a GitHub OAuth App; see
[`docs/todo/github-device-flow.md`](./todo/github-device-flow.md)
for the restoration plan.

### Create the PAT

1. Open <https://github.com/settings/tokens> in your browser.
2. Click **Generate new token** and pick either:
   - **Tokens (classic)** — simpler; pick the `repo` scope (and
     nothing else for MVP).
   - **Fine-grained tokens** — more granular; grant **Contents:
     Read and write** plus **Metadata: Read** on each repository
     you want Tartarus to clone. (You can also grant access to all
     of your repos, but resource-scoped is the safer default.)
3. Set an expiry. Tartarus has no rotation tooling yet; pick the
   shortest expiry you can live with re-creating manually.
4. Click **Generate token** and copy the value. GitHub shows it
   once.

### What scope does what

| Scope | Why Tartarus needs it |
|---|---|
| `repo` (classic) or **Contents: Read and write** (fine-grained) | `git clone` over HTTPS, push commits back to feature branches. |
| **Metadata: Read** (fine-grained only — implicit on classic) | `gh` needs it for repo lookup. |
| `workflow` *(optional)* | Only if your agent will edit files under `.github/workflows/`. Tartarus does not need it for clone. |

### Hand it to Tartarus

```console
$ tartarus auth init
GitHub personal access token (paste): ghp_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx
Anthropic API key (paste, or press Enter for browser): ...
wrote ~/.config/tartarus/config.toml (mode 0600)
```

Tokens beginning with `ghp_` (classic) and `github_pat_`
(fine-grained) are the recognised prefixes; anything else is
accepted with a `tracing::warn!` so future GitHub format changes do
not lock you out.

### What can go wrong

- **Empty input.** Hitting Enter at the prompt produces:

  ```
  error: no GitHub personal access token was provided.
  hint:  create one at https://github.com/settings/tokens with the
         `repo` scope, then re-run `tartarus auth init` and paste
         it at the prompt.
  ```

- **Token does not match a known prefix.** Tartarus warns and
  accepts anyway:

  ```
  WARN the supplied GitHub token does not match any known prefix
       (["ghp_", "github_pat_"]); accepting anyway
  ```

- **Token rejected at clone time.** The bootstrap inside the guest
  fails with `gh auth login` reporting the rejection. Re-run
  `tartarus auth init --force` to overwrite the token.

---

## 2. Anthropic API key (default backend)

The default Claude backend is **Anthropic**: you paste an API key,
the in-guest agent picks it up via `ANTHROPIC_API_KEY`, and Claude
talks to `api.anthropic.com` directly.

### Create the key

1. Open <https://console.anthropic.com/settings/keys>.
2. Click **Create Key**, name it (e.g. `tartarus-laptop`), and
   confirm. Keys are shown once.
3. Copy the value (starts with `sk-ant-`).

If you skip this step, `tartarus auth init` will open the URL for
you and re-prompt:

```console
$ tartarus auth init
GitHub personal access token (paste): ghp_...
Anthropic API key (paste, or press Enter for browser): [Enter]
Opening https://console.anthropic.com/settings/keys (Anthropic has no public OAuth flow for API keys)
Anthropic API key (paste): sk-ant-...
wrote ~/.config/tartarus/config.toml (mode 0600)
```

### Verify it's wired

```console
$ tartarus auth status
GitHub:    configured (last 4: …WXYZ)
Backend:   anthropic
Anthropic: configured (last 4: …vMno)
Vertex:    not configured
```

`auth status` redacts every secret to its last four characters per
the spec. A key configured here is sufficient to start a session
once the base image is in place.

### What can go wrong

- **Empty paste at both prompts.** The flow refuses with
  `error: no Anthropic API key was provided` rather than silently
  writing a key-less config.
- **Wrong account / disabled key.** The agent fails with
  `claude` reporting an authentication error. Re-run with
  `--force` and a working key.

---

## 3. Vertex AI service account (alternative backend)

Vertex routes Claude through Google Cloud's
`anthropic.claude-*` Vertex models. You provide a service-account
file, project ID, and region; the file is read on the host and
copied into each session at first boot at `0600` under
`/etc/tartarus/google-credentials.json`.

> **Pre-flight check.** You need a GCP project with the **Vertex
> AI** API enabled, an active billing account, and `gcloud` (or the
> Cloud Console) to create the service account.

### Create the service account

```console
# Pick a project (use an existing one):
gcloud config set project YOUR-PROJECT-ID

# Enable Vertex AI on this project (one-time):
gcloud services enable aiplatform.googleapis.com

# Create the service account:
gcloud iam service-accounts create tartarus-claude \
  --display-name="Tartarus Claude (Vertex)"

# Grant the minimum role for invoking Claude on Vertex:
gcloud projects add-iam-policy-binding YOUR-PROJECT-ID \
  --member="serviceAccount:tartarus-claude@YOUR-PROJECT-ID.iam.gserviceaccount.com" \
  --role="roles/aiplatform.user"

# Mint a key file and place it where Tartarus can read it:
mkdir -p ~/.config/tartarus
gcloud iam service-accounts keys create \
  ~/.config/tartarus/vertex-sa.json \
  --iam-account="tartarus-claude@YOUR-PROJECT-ID.iam.gserviceaccount.com"

# Tighten the permissions (gcloud usually does this, but defense in depth):
chmod 0600 ~/.config/tartarus/vertex-sa.json
```

The role to grant is `roles/aiplatform.user` — Vertex AI User. It
covers `aiplatform.endpoints.predict`, which is what Claude on
Vertex needs. Avoid the broader `roles/aiplatform.admin`.

### Pick a region

Anthropic on Vertex is available in a fixed list of regions. The
defaults that worked at the time of writing are `us-east5` and
`europe-west1`; the
[Anthropic Vertex docs](https://docs.anthropic.com/en/api/claude-on-vertex-ai)
have the current list. The Tartarus default is `us-east5`.

### Hand it to Tartarus

```console
$ tartarus auth init google
GCP project ID: YOUR-PROJECT-ID
Region [us-east5]: [Enter]
Service-account JSON file (absolute path): /home/alice/.config/tartarus/vertex-sa.json
wrote ~/.config/tartarus/config.toml (mode 0600)
```

`auth init google` **merges** into your existing config — it does
not blow away your GitHub PAT or your Anthropic API key.
After it runs, `[claude] backend = "vertex"` is the active backend
and Anthropic falls dormant (the API key is preserved in the file
but unused; you can flip back by editing
`[claude] backend = "anthropic"`).

### Verify it's wired

```console
$ tartarus auth status
GitHub:    configured (last 4: …WXYZ)
Backend:   vertex
Vertex:    configured
  project: YOUR-PROJECT-ID
  region:  us-east5
  file:    /home/alice/.config/tartarus/vertex-sa.json
```

### What can go wrong

- **Path is not absolute.** Tartarus does not expand `~` or
  environment variables. Pass the absolute path.
- **File is unreadable.** Mode `0400`/`0600` are fine; group- or
  world-readable files will still load (Tartarus does not refuse
  them) but `tartarus doctor` will warn.
- **JSON is invalid.** `auth init google` validates the file
  parses. A truncated download or an editor's BOM trips this.
- **Region mismatch.** Claude on Vertex returns a 404 if the
  requested model is not enabled in the chosen region. Either
  switch region or enable the model in the GCP console.

---

## 4. Backend switching after the fact

The two `auth init` subcommands compose. Common shapes:

### Anthropic-first, then add Vertex

```console
tartarus auth init           # GitHub + Anthropic
tartarus auth init google    # adds Vertex; flips backend to vertex
```

To go back to Anthropic without re-typing the API key, hand-edit
the active backend in `~/.config/tartarus/config.toml`:

```toml
[claude]
backend = "anthropic"
```

Both credential bundles remain in the file; only the active backend
is read at `tartarus run` time.

### Replace credentials wholesale

```console
tartarus auth init --force
```

`--force` overwrites `config.toml` entirely. Use it when you need
to rotate a leaked PAT or change accounts. Vertex credentials in
the file are dropped — re-run `tartarus auth init google` after
to put them back.

---

## 5. Where the credentials live

| Path | Mode | Contents |
|---|---|---|
| `~/.config/tartarus/config.toml` | `0600` | Active backend + GitHub PAT + Anthropic key + Vertex paths/IDs. |
| `~/.config/tartarus/vertex-sa.json` *(suggested)* | `0600` | The Vertex service-account JSON. Path is configurable; this is the convention. |
| `~/.local/share/tartarus/sessions/by-uuid/<uuid>/cloud-init.iso` | `0600` | Per-session seed ISO. Carries the same credentials, base64-cleanly embedded into a NoCloud datasource. Removed on `tartarus destroy <session>`. |
| `~/.local/share/tartarus/sessions/by-uuid/<uuid>/user-data` | `0600` | Rendered cloud-init source kept alongside the ISO for audit. Same secrets. |
| `~/.local/share/tartarus/sessions/by-uuid/<uuid>/metadata.json` | `0600` | Session metadata, including the background-mode Claude remote-connect URL when applicable. |
| `~/.local/share/tartarus/base/fedora.gpg` | default | The Fedora release key Tartarus pinned on the very first `tartarus base pull`. |
| `~/.local/share/tartarus/base/fedora.gpg.fingerprints` | default | Trust-on-first-use fingerprint pin checked on every subsequent pull. |

The session directory itself is mode `0700`, so the per-file modes
matter only on hosts where `$HOME` is more permissive than `0700`
(some shared dev boxes). The session secrets persist on disk for
the lifetime of the session — `tartarus destroy <session>` is the
only way to scrub them.

---

## 6. Verifying the setup end-to-end

Once `auth status` reports your backend as configured:

```console
# Confirm libvirtd, /dev/kvm, and the helper tools:
tartarus doctor

# Pull and lay the base image (long; see quickstart.md):
tartarus base pull

# Smoke-test against a tiny repo:
tartarus run --repo your/test-repo --detach
tartarus list
tartarus destroy your-test-session-uuid
```

`tartarus doctor` is the single source of truth for "is this host
ready to run a session?" — it covers the auth posture, libvirtd
reachability, KVM access, GPG trust anchor, and the helper-tool
PATH lookups.

---

## 7. Rotating credentials

There is no automated rotation in MVP. To replace any credential:

1. Generate the new value in the upstream console (GitHub /
   Anthropic / GCP).
2. Run `tartarus auth init --force` (Anthropic) or
   `tartarus auth init google` (Vertex). Existing fields you don't
   re-supply are preserved by `init google`'s merge semantics;
   `init --force` overwrites everything except what you re-paste,
   so re-paste both.
3. **Existing sessions keep the old credentials** — they were
   baked into the seed ISO at boot time. Re-create the session
   (`tartarus destroy` + `tartarus run`) to pick up the rotated
   values. cloud-init's `instance-id == session UUID` contract
   means the same session does not re-run `write_files`.

---

## 8. Threat model and what's protected

- **Other local users on a shared host.** Mitigated by `0600` on
  every credential-bearing file plus `0700` on the session
  directory. A user-space process running as the same UID can
  still read everything Tartarus reads.
- **A compromised guest.** The seed ISO is presented to the guest
  as a CD-ROM at boot; once cloud-init has read it, the credentials
  are inside the guest's filesystem. The guest is the trust
  boundary — anything inside can read these credentials. This is
  the design intent: the agent inside the VM is the one calling
  Claude.
- **A compromised mirror.** Image and CHECKSUM downloads are
  TLS-strict (`https_only = true`, `rustls`-only). The Fedora
  release key is fetched on first pull and pinned via a TOFU
  fingerprint sidecar; later pulls reuse the persisted key rather
  than re-fetching.
- **A leaked PAT or API key.** Rotate per §7, then `tartarus
  destroy` every existing session that booted with the old
  credential. There is no per-session revocation path beyond
  destroying the session.

For the full layered security review that drove the current
posture, see the post-review reports referenced in
`docs/todo/`.
