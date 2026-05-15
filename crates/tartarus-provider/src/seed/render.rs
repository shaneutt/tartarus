//! Render a [`crate::seed::input::Seed`] into cloud-init `user-data` and
//! `meta-data`.
//!
//! Pure (data in, strings out). The bootstrap timeline follows
//! [`docs/spec.md`]'s "Bootstrap Flow".
//!
//! [`docs/spec.md`]: https://github.com/the-lost-art-of-programming/tartarus/blob/main/docs/spec.md

use crate::{
    host_user,
    seed::input::{CredentialBundle, Seed},
};

// -----------------------------------------------------------------------------
// Constants
// -----------------------------------------------------------------------------

/// Per-session env file directory on the guest.
const ENV_DIR: &str = "/run/tartarus/env";

/// On-guest path of the pre-staged Claude Code tarball.
const CLAUDE_TARBALL_PATH: &str = "/opt/tartarus/skeleton/claude-code.tgz";

/// On-guest Vertex credentials JSON path (mode 0600).
const VERTEX_CREDS_PATH: &str = "/etc/tartarus/google-credentials.json";

/// Seeded-user discovery file on the guest.
const TARTARUS_USER_FILE_PATH: &str = "/etc/tartarus/tartarus-user";

/// Repo manifest consumed by the in-guest bootstrap.
const REPOS_MANIFEST_PATH: &str = "/etc/tartarus/repos";

// -----------------------------------------------------------------------------
// SeedDocuments
// -----------------------------------------------------------------------------

/// Render result: `user-data` + `meta-data` as in-memory strings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SeedDocuments {
    /// Contents of cloud-init `meta-data`.
    pub meta_data: String,

    /// Contents of cloud-init `user-data` (`#cloud-config` YAML).
    pub user_data: String,
}

/// Render `seed` into a [`SeedDocuments`]. Pure (no filesystem I/O).
pub fn render(seed: &Seed) -> SeedDocuments {
    let meta_data = render_meta_data(seed);
    let user_data = render_user_data(seed);

    SeedDocuments { meta_data, user_data }
}

// -----------------------------------------------------------------------------
// Cloud-Init Rendering
// -----------------------------------------------------------------------------

/// Render cloud-init `meta-data` (instance-id + local-hostname).
fn render_meta_data(seed: &Seed) -> String {
    format!(
        "instance-id: {uuid}\nlocal-hostname: {host}\n",
        uuid = seed.uuid,
        host = local_hostname(seed)
    )
}

/// Derive local-hostname from alias or UUID prefix.
fn local_hostname(seed: &Seed) -> String {
    if seed.name == "(unnamed)" {
        let prefix: String = seed.uuid.chars().take(8).collect();
        format!("tartarus-{prefix}")
    } else {
        format!("tartarus-{name}", name = sanitize_hostname(&seed.name))
    }
}

/// Sanitise to `[a-z0-9-]` for a valid DNS label.
fn sanitize_hostname(input: &str) -> String {
    input
        .chars()
        .map(|c| match c {
            'a'..='z' | '0'..='9' | '-' => c,
            'A'..='Z' => c.to_ascii_lowercase(),
            _ => '-',
        })
        .collect()
}

/// Render the `user-data` `#cloud-config` YAML.
fn render_user_data(seed: &Seed) -> String {
    let mut out = String::new();
    out.push_str("#cloud-config\n");
    out.push_str("# Tartarus session seed (generated). Per-session credentials and repos.\n\n");

    push_growpart(&mut out);
    push_users(&mut out, seed);
    push_write_files(&mut out, seed);
    push_runcmd(&mut out, seed);

    out
}

/// Append the `growpart` + `resize_rootfs` block.
fn push_growpart(out: &mut String) {
    out.push_str("growpart:\n");
    out.push_str("  mode: auto\n");
    out.push_str("  devices: ['/']\n");
    out.push_str("  ignore_growroot_disabled: false\n");
    out.push_str("resize_rootfs: true\n\n");
}

/// Append the `users:` block (unprivileged, matching host identity).
fn push_users(out: &mut String, seed: &Seed) {
    out.push_str("users:\n");
    out.push_str("  - name: ");
    out.push_str(&seed.user.username);
    out.push('\n');
    out.push_str(&format!("    uid: '{uid}'\n", uid = seed.user.uid));
    out.push_str(&format!("    gid: '{gid}'\n", gid = seed.user.gid));
    out.push_str("    lock_passwd: true\n");
    out.push_str("    shell: /bin/bash\n");
    if let Some(pubkey) = seed.ssh_pubkey.as_deref() {
        let trimmed = pubkey.trim();
        if crate::seed::input::is_safe_single_line(trimmed) {
            out.push_str("    ssh_authorized_keys:\n");
            out.push_str("      - ");
            out.push_str(trimmed);
            out.push('\n');
        } else {
            tracing::warn!("ssh pubkey failed single-line validation; skipping authorized_keys injection");
        }
    }
    out.push('\n');
}

/// Append the `write_files:` block (env files, Vertex creds, repo
/// manifest, systemd drop-in).
fn push_write_files(out: &mut String, seed: &Seed) {
    out.push_str("write_files:\n");

    push_tartarus_user_file(out, seed);
    push_env_files(out, seed);
    push_vertex_creds_if_any(out, seed);
    push_repos_manifest(out, seed);
    push_bootstrap_user_dropin(out, seed);

    out.push('\n');
}

/// Append the `tartarus-user` discovery file.
fn push_tartarus_user_file(out: &mut String, seed: &Seed) {
    let body = format!("{name}\n", name = seed.user.username);
    emit_write_file(out, TARTARUS_USER_FILE_PATH, &body, "0644", "root:root");
}

/// Append the systemd `User=` drop-in for `tartarus-bootstrap.service`.
fn push_bootstrap_user_dropin(out: &mut String, seed: &Seed) {
    let user = &seed.user.username;
    let body = format!("[Service]\nUser={user}\nGroup={user}\n");
    emit_write_file(
        out,
        "/etc/systemd/system/tartarus-bootstrap.service.d/user.conf",
        &body,
        "0644",
        "root:root",
    );
}

/// Append credential and Claude-default env files.
fn push_env_files(out: &mut String, seed: &Seed) {
    let owner = format!("{name}:{name}", name = seed.user.username);

    emit_env_file(out, "GITHUB_TOKEN", &seed.credentials.github_token, &owner);
    emit_env_file(out, "CLAUDE_MODEL", &seed.credentials.claude.model, &owner);
    emit_env_file(out, "CLAUDE_EFFORT", &seed.credentials.claude.effort, &owner);

    match &seed.credentials.backend {
        CredentialBundle::Anthropic { api_key } => {
            emit_env_file(out, "ANTHROPIC_API_KEY", api_key, &owner);
        },
        CredentialBundle::Vertex { project_id, region, .. } => {
            emit_env_file(out, "CLAUDE_CODE_USE_VERTEX", "1", &owner);
            emit_env_file(out, "GOOGLE_APPLICATION_CREDENTIALS", VERTEX_CREDS_PATH, &owner);
            emit_env_file(out, "CLOUD_ML_REGION", region, &owner);
            emit_env_file(out, "ANTHROPIC_VERTEX_PROJECT_ID", project_id, &owner);
        },
    }

    if seed.remote_connect {
        // TODO: replace with the published Claude Code remote-connect env contract.
        // P6 picks placeholder names so the seam is grep-friendly; the maintainer
        // swaps `CLAUDE_REMOTE_ENABLED` and the matching token plumbing in
        // `host::agent` once the feature stabilises.
        emit_env_file(out, "CLAUDE_REMOTE_ENABLED", "1", &owner);
    }
}

/// Append the Vertex SA JSON file (mode 0600) if applicable.
fn push_vertex_creds_if_any(out: &mut String, seed: &Seed) {
    let CredentialBundle::Vertex { credentials_json, .. } = &seed.credentials.backend else {
        return;
    };

    let owner = format!("{name}:{name}", name = seed.user.username);
    emit_write_file(out, VERTEX_CREDS_PATH, credentials_json, "0600", &owner);
}

/// Append the `/etc/tartarus/repos` manifest.
fn push_repos_manifest(out: &mut String, seed: &Seed) {
    let owner = format!("{name}:{name}", name = seed.user.username);
    let mut body = String::new();
    for repo in &seed.repos {
        let default = if repo.default { "default" } else { "" };
        body.push_str(&format!("{slug}\t{default}\n", slug = repo.slug));
    }
    emit_write_file(out, REPOS_MANIFEST_PATH, &body, "0644", &owner);
}

/// Append `runcmd:` (env-dir prep, Claude install, bootstrap trigger).
fn push_runcmd(out: &mut String, seed: &Seed) {
    let home = host_user::home_dir(&seed.user);
    let user = &seed.user.username;

    out.push_str("runcmd:\n");
    out.push_str(&format!("  - mkdir -p {ENV_DIR}\n"));
    out.push_str(&format!("  - chown -R {user}:{user} {ENV_DIR}\n"));
    out.push_str(&format!("  - chmod 0700 {ENV_DIR}\n"));
    out.push_str(&format!(
        "  - install -d -o {user} -g {user} {home}/.local\n",
        home = home.display()
    ));
    out.push_str(&format!(
        "  - sudo -u {user} -H npm install --prefix={home}/.local {tarball}\n",
        home = home.display(),
        tarball = CLAUDE_TARBALL_PATH,
    ));
    out.push_str("  - systemctl start tartarus-bootstrap.service\n");
}

/// Append one env-file entry under [`ENV_DIR`].
fn emit_env_file(out: &mut String, name: &str, value: &str, owner: &str) {
    let path = format!("{ENV_DIR}/{name}");
    let body = format!("{name}={value}\n");
    emit_write_file(out, &path, &body, "0600", owner);
}

/// Append one `write_files` entry using a YAML `|` block-scalar.
///
/// NUL and `\r` are stripped as defense-in-depth.
fn emit_write_file(out: &mut String, path: &str, body: &str, permissions: &str, owner: &str) {
    out.push_str(&format!("  - path: {path}\n"));
    out.push_str(&format!("    permissions: '{permissions}'\n"));
    out.push_str(&format!("    owner: {owner}\n"));
    out.push_str("    content: |\n");
    for line in body.lines() {
        out.push_str("      ");
        for c in line.chars() {
            if c != '\0' && c != '\r' {
                out.push(c);
            }
        }
        out.push('\n');
    }
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        host_user::HostUser,
        seed::input::{ClaudeDefaults, CredentialBundle, Credentials, RepoSpec, Seed},
    };

    #[test]
    fn meta_data_carries_uuid_and_hostname() {
        let seed = anthropic_seed("fix-bug");
        let docs = render(&seed);

        assert!(
            docs.meta_data
                .contains("instance-id: 11111111-2222-3333-4444-555555555555"),
            "meta-data should carry the session UUID, got: {meta}",
            meta = docs.meta_data,
        );
        assert!(
            docs.meta_data.contains("local-hostname: tartarus-fix-bug"),
            "meta-data should derive hostname from the alias, got: {meta}",
            meta = docs.meta_data,
        );
    }

    #[test]
    fn user_data_starts_with_cloud_config_marker() {
        let seed = anthropic_seed("(unnamed)");
        let docs = render(&seed);

        assert!(
            docs.user_data.starts_with("#cloud-config\n"),
            "user-data must start with the magic comment for cloud-init to parse it",
        );
    }

    #[test]
    fn user_data_creates_user_at_invoker_uid_and_gid() {
        let seed = anthropic_seed("(unnamed)");
        let docs = render(&seed);

        assert!(
            docs.user_data.contains("- name: alice"),
            "users: should name the invoker"
        );
        assert!(
            docs.user_data.contains("uid: '1000'"),
            "uid should be quoted to keep it numeric"
        );
        assert!(
            docs.user_data.contains("gid: '1000'"),
            "gid should be quoted to keep it numeric"
        );
        assert!(
            docs.user_data.contains("lock_passwd: true"),
            "the in-guest user has no password (sudo not granted in MVP)",
        );
    }

    #[test]
    fn user_data_writes_tartarus_user_file_with_seeded_username() {
        let seed = anthropic_seed("alice");
        let docs = render(&seed);

        assert!(
            docs.user_data.contains("path: /etc/tartarus/tartarus-user"),
            "user-data should declare the tartarus-user discovery file path; got: {user_data}",
            user_data = docs.user_data,
        );
        assert!(
            docs.user_data.contains("      alice\n"),
            "tartarus-user file body should carry the seeded username; got: {user_data}",
            user_data = docs.user_data,
        );
    }

    #[test]
    fn user_data_drops_anthropic_env_files() {
        let seed = anthropic_seed("(unnamed)");
        let docs = render(&seed);

        for name in ["GITHUB_TOKEN", "CLAUDE_MODEL", "CLAUDE_EFFORT", "ANTHROPIC_API_KEY"] {
            assert!(
                docs.user_data.contains(&format!("{ENV_DIR}/{name}")),
                "user-data should drop env file {name}, got: {user_data}",
                user_data = docs.user_data,
            );
        }
        assert!(
            docs.user_data.contains("permissions: '0600'"),
            "env files must be mode 0600",
        );
    }

    #[test]
    fn user_data_drops_repos_manifest() {
        let seed = anthropic_seed("(unnamed)");
        let docs = render(&seed);

        assert!(
            docs.user_data.contains(REPOS_MANIFEST_PATH),
            "user-data should drop the repos manifest, got: {user_data}",
            user_data = docs.user_data,
        );
        assert!(
            docs.user_data.contains("owner/name\tdefault"),
            "manifest body should include the slug + default marker, got: {user_data}",
            user_data = docs.user_data,
        );
    }

    #[test]
    fn user_data_runs_npm_install_as_user() {
        let seed = anthropic_seed("(unnamed)");
        let docs = render(&seed);

        assert!(
            docs.user_data.contains(
                "sudo -u alice -H npm install --prefix=/home/alice/.local /opt/tartarus/skeleton/claude-code.tgz"
            ),
            "npm install must run as the user against the per-user prefix, got: {user_data}",
            user_data = docs.user_data,
        );
    }

    #[test]
    fn user_data_triggers_bootstrap_service() {
        let seed = anthropic_seed("(unnamed)");
        let docs = render(&seed);

        assert!(
            docs.user_data.contains("systemctl start tartarus-bootstrap.service"),
            "user-data must trigger the in-guest bootstrap service, got: {user_data}",
            user_data = docs.user_data,
        );
    }

    #[test]
    fn user_data_drops_systemd_dropin_pinning_bootstrap_user() {
        let seed = anthropic_seed("(unnamed)");
        let docs = render(&seed);

        assert!(
            docs.user_data
                .contains("/etc/systemd/system/tartarus-bootstrap.service.d/user.conf"),
            "user-data should drop a systemd User= override for tartarus-bootstrap.service, got: {user_data}",
            user_data = docs.user_data,
        );
        assert!(
            docs.user_data.contains("User=alice"),
            "drop-in should pin User= to the cloud-init-created user, got: {user_data}",
            user_data = docs.user_data,
        );
    }

    #[test]
    fn user_data_injects_remote_connect_env_when_enabled() {
        let mut seed = anthropic_seed("(unnamed)");
        seed.remote_connect = true;
        let docs = render(&seed);

        assert!(
            docs.user_data.contains(&format!("{ENV_DIR}/CLAUDE_REMOTE_ENABLED")),
            "background-mode seed should drop the CLAUDE_REMOTE_ENABLED env file, got: {user_data}",
            user_data = docs.user_data,
        );
        assert!(
            docs.user_data.contains("CLAUDE_REMOTE_ENABLED=1"),
            "remote-connect env file should set the value to 1, got: {user_data}",
            user_data = docs.user_data,
        );
    }

    #[test]
    fn user_data_omits_remote_connect_env_when_disabled() {
        let seed = anthropic_seed("(unnamed)");
        let docs = render(&seed);

        assert!(
            !docs.user_data.contains("CLAUDE_REMOTE_ENABLED"),
            "non-background sessions must not carry the remote-connect env file, got: {user_data}",
            user_data = docs.user_data,
        );
    }

    #[test]
    fn user_data_does_not_grant_sudo() {
        let seed = anthropic_seed("(unnamed)");
        let docs = render(&seed);

        assert!(
            !docs.user_data.contains("sudo:"),
            "MVP user must not have sudo granted, got: {user_data}",
            user_data = docs.user_data,
        );
    }

    #[test]
    fn vertex_seed_emits_credentials_and_env_files() {
        let seed = vertex_seed("{\"type\":\"service_account\"}");
        let docs = render(&seed);

        assert!(
            docs.user_data.contains(VERTEX_CREDS_PATH),
            "vertex seed should drop the credentials JSON onto the guest, got: {user_data}",
            user_data = docs.user_data,
        );
        for name in [
            "CLAUDE_CODE_USE_VERTEX",
            "GOOGLE_APPLICATION_CREDENTIALS",
            "CLOUD_ML_REGION",
            "ANTHROPIC_VERTEX_PROJECT_ID",
        ] {
            assert!(
                docs.user_data.contains(&format!("{ENV_DIR}/{name}")),
                "vertex seed should drop env file {name}, got: {user_data}",
                user_data = docs.user_data,
            );
        }
    }

    #[test]
    fn vertex_seed_writes_credentials_at_mode_0600() {
        let seed = vertex_seed("{\"type\":\"service_account\"}");
        let docs = render(&seed);

        let needle = format!("path: {VERTEX_CREDS_PATH}\n    permissions: '0600'");
        assert!(
            docs.user_data.contains(&needle),
            "vertex SA file must be mode 0600, got: {user_data}",
            user_data = docs.user_data,
        );
        assert!(
            docs.user_data.contains("owner: alice:alice"),
            "vertex SA file must be owned by the in-guest user, got: {user_data}",
            user_data = docs.user_data,
        );
    }

    #[test]
    fn vertex_seed_does_not_write_anthropic_api_key() {
        let seed = vertex_seed("{\"type\":\"service_account\"}");
        let docs = render(&seed);

        assert!(
            !docs.user_data.contains(&format!("{ENV_DIR}/ANTHROPIC_API_KEY")),
            "vertex seed must not write the Anthropic API key (backends are exclusive), got: {user_data}",
            user_data = docs.user_data,
        );
    }

    #[test]
    fn anthropic_seed_does_not_write_vertex_env_files() {
        let seed = anthropic_seed("(unnamed)");
        let docs = render(&seed);

        for name in [
            "CLAUDE_CODE_USE_VERTEX",
            "GOOGLE_APPLICATION_CREDENTIALS",
            "CLOUD_ML_REGION",
            "ANTHROPIC_VERTEX_PROJECT_ID",
        ] {
            assert!(
                !docs.user_data.contains(&format!("{ENV_DIR}/{name}")),
                "anthropic seed must not write Vertex env file {name} (backends are exclusive), got: {user_data}",
                user_data = docs.user_data,
            );
        }
        assert!(
            !docs.user_data.contains(VERTEX_CREDS_PATH),
            "anthropic seed must not drop the Vertex SA file, got: {user_data}",
            user_data = docs.user_data,
        );
    }

    #[test]
    fn user_data_renders_multi_repo_manifest_with_one_default() {
        let mut seed = anthropic_seed("(unnamed)");
        seed.repos = vec![
            crate::seed::input::RepoSpec {
                default: false,
                slug: "owner/alpha".to_owned(),
            },
            crate::seed::input::RepoSpec {
                default: true,
                slug: "owner/beta".to_owned(),
            },
            crate::seed::input::RepoSpec {
                default: false,
                slug: "owner/gamma".to_owned(),
            },
        ];

        let docs = render(&seed);

        assert!(
            docs.user_data.contains("owner/alpha\t\n"),
            "non-default repo line should carry an empty flag, got: {user_data}",
            user_data = docs.user_data,
        );
        assert!(
            docs.user_data.contains("owner/beta\tdefault\n"),
            "default-flagged repo should be tagged `default`, got: {user_data}",
            user_data = docs.user_data,
        );
        assert!(
            docs.user_data.contains("owner/gamma\t\n"),
            "non-default repo line should carry an empty flag, got: {user_data}",
            user_data = docs.user_data,
        );
        let default_count = docs
            .user_data
            .lines()
            .filter(|line| line.trim_start().ends_with("\tdefault"))
            .count();
        assert_eq!(
            default_count,
            1,
            "exactly one repo should carry the `default` flag in the manifest, got: {user_data}",
            user_data = docs.user_data,
        );
    }

    #[test]
    fn meta_data_for_unnamed_session_uses_uuid_prefix() {
        let seed = anthropic_seed("(unnamed)");
        let docs = render(&seed);

        assert!(
            docs.meta_data.contains("local-hostname: tartarus-11111111"),
            "unnamed session hostname should derive from the UUID prefix, got: {meta}",
            meta = docs.meta_data,
        );
    }

    #[test]
    fn user_data_has_well_formed_yaml_structure() {
        let seed = anthropic_seed("(unnamed)");
        let docs = render(&seed);
        let lines: Vec<&str> = docs.user_data.lines().collect();

        for top_level in ["users:", "write_files:", "runcmd:"] {
            assert!(
                lines.contains(&top_level),
                "user-data should carry the top-level YAML key `{top_level}`, got:\n{ud}",
                ud = docs.user_data,
            );
        }
        for line in &lines {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let indent = line.chars().take_while(|c| *c == ' ').count();
            assert!(
                indent.is_multiple_of(2),
                "YAML indentation must be a multiple of 2, got {indent} on line: {line}",
            );
        }
    }

    #[test]
    fn sanitize_hostname_replaces_unsafe_characters() {
        assert_eq!(sanitize_hostname("Fix-Bug_123"), "fix-bug-123");
        assert_eq!(sanitize_hostname("ALL CAPS"), "all-caps");
    }

    #[test]
    fn user_data_carries_growpart_directive() {
        let seed = anthropic_seed("(unnamed)");
        let docs = render(&seed);

        assert!(
            docs.user_data.contains("growpart:"),
            "user-data should carry the cloud-init growpart directive, got: {ud}",
            ud = docs.user_data,
        );
        assert!(
            docs.user_data.contains("mode: auto"),
            "growpart should request auto mode (Fedora ships growpart.conf with mode=auto), got: {ud}",
            ud = docs.user_data,
        );
        assert!(
            docs.user_data.contains("devices: ['/']"),
            "growpart should target the root mount, got: {ud}",
            ud = docs.user_data,
        );
        assert!(
            docs.user_data.contains("resize_rootfs: true"),
            "user-data should request rootfs resize alongside growpart, got: {ud}",
            ud = docs.user_data,
        );
    }

    #[test]
    fn growpart_directive_precedes_bootstrap_trigger() {
        let seed = anthropic_seed("(unnamed)");
        let docs = render(&seed);

        let growpart_idx = docs.user_data.find("growpart:").expect("growpart present");
        let bootstrap_idx = docs
            .user_data
            .find("systemctl start tartarus-bootstrap.service")
            .expect("bootstrap trigger present");

        assert!(
            growpart_idx < bootstrap_idx,
            "growpart directive must appear before tartarus-bootstrap is triggered so the rootfs is full-size first",
        );
    }

    // -----------------------------------------------------------------------
    // Test Utilities
    // -----------------------------------------------------------------------

    fn sample_user() -> HostUser {
        HostUser {
            gid: 1000,
            uid: 1000,
            username: "alice".to_owned(),
        }
    }

    fn anthropic_seed(name: &str) -> Seed {
        Seed {
            name: name.to_owned(),
            credentials: Credentials {
                backend: CredentialBundle::Anthropic {
                    api_key: "sk-ant-test".to_owned(),
                },
                claude: ClaudeDefaults {
                    effort: "high".to_owned(),
                    model: "claude-opus-4-7".to_owned(),
                },
                github_token: "ghp_test".to_owned(),
            },
            envs: vec!["rust".to_owned()],
            remote_connect: false,
            repos: vec![RepoSpec {
                slug: "owner/name".to_owned(),
                default: true,
            }],
            ssh_pubkey: None,
            uuid: "11111111-2222-3333-4444-555555555555".to_owned(),
            user: sample_user(),
        }
    }

    fn vertex_seed(credentials_json: &str) -> Seed {
        Seed {
            name: "(unnamed)".to_owned(),
            credentials: Credentials {
                backend: CredentialBundle::Vertex {
                    credentials_json: credentials_json.to_owned(),
                    project_id: "my-project".to_owned(),
                    region: "us-east5".to_owned(),
                },
                claude: ClaudeDefaults {
                    effort: "high".to_owned(),
                    model: "claude-opus-4-7".to_owned(),
                },
                github_token: "ghp_test".to_owned(),
            },
            envs: vec![],
            remote_connect: false,
            repos: vec![RepoSpec {
                slug: "owner/name".to_owned(),
                default: true,
            }],
            ssh_pubkey: None,
            uuid: "11111111-2222-3333-4444-555555555555".to_owned(),
            user: sample_user(),
        }
    }

    fn assert_yaml_well_formed(yaml: &str) {
        if let Err(reason) = check_yaml_structure(yaml) {
            panic!("rendered YAML failed structural validation: {reason}\n---\n{yaml}");
        }
    }

    fn check_yaml_structure(yaml: &str) -> std::result::Result<(), String> {
        let mut top_keys: Vec<&str> = Vec::new();
        let mut in_block_scalar = false;
        let mut block_scalar_indent: Option<usize> = None;

        for (lineno, raw) in yaml.lines().enumerate() {
            let lineno = lineno + 1;

            if raw.trim().is_empty() || raw.trim_start().starts_with('#') {
                continue;
            }

            let indent = raw.bytes().take_while(|b| *b == b' ').count();

            if in_block_scalar {
                let parent = block_scalar_indent.unwrap_or(0);
                if indent <= parent && !raw.trim().is_empty() {
                    in_block_scalar = false;
                    block_scalar_indent = None;
                } else {
                    continue;
                }
            }

            let trimmed = raw.trim_start();

            if !is_quote_balanced(trimmed) {
                return Err(format!("line {lineno}: unterminated quoted string: {trimmed:?}"));
            }

            if indent == 0
                && let Some(colon) = trimmed.find(':')
                && colon > 0
            {
                let key = &trimmed[..colon];
                if top_keys.contains(&key) {
                    return Err(format!("line {lineno}: duplicate top-level key {key:?}"));
                }
                top_keys.push(key);
            }

            if trimmed.ends_with("|") || trimmed.ends_with("|-") || trimmed.ends_with(">") {
                in_block_scalar = true;
                block_scalar_indent = Some(indent);
            }
        }

        Ok(())
    }

    fn is_quote_balanced(line: &str) -> bool {
        let mut single = 0_u32;
        let mut double = 0_u32;
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\'' if double.is_multiple_of(2) => {
                    if chars.peek() == Some(&'\'') {
                        chars.next();
                        continue;
                    }
                    single += 1;
                },
                '"' if single.is_multiple_of(2) => {
                    double += 1;
                },
                '#' if single.is_multiple_of(2) && double.is_multiple_of(2) => {
                    return true;
                },
                _ => {},
            }
        }
        single.is_multiple_of(2) && double.is_multiple_of(2)
    }

    #[test]
    fn anthropic_seed_renders_structurally_well_formed_yaml() {
        let seed = anthropic_seed("alice");
        let docs = render(&seed);

        assert_yaml_well_formed(&docs.user_data);
    }

    #[test]
    fn vertex_seed_renders_structurally_well_formed_yaml() {
        let seed = vertex_seed("{\"client_email\":\"x@y\"}");
        let docs = render(&seed);

        assert_yaml_well_formed(&docs.user_data);
    }

    #[test]
    fn check_yaml_structure_rejects_duplicate_top_level_keys() {
        let yaml = "users:\n  - name: x\nusers:\n  - name: y\n";
        let err = check_yaml_structure(yaml).expect_err("duplicate top-level keys should fail");
        assert!(err.contains("duplicate"), "error should name the failure mode: {err}");
    }

    #[test]
    fn check_yaml_structure_rejects_unterminated_quoted_string() {
        let yaml = "key: 'unterminated\nother: ok\n";
        let err = check_yaml_structure(yaml).expect_err("unterminated quote should fail");
        assert!(
            err.contains("unterminated"),
            "error should name the failure mode: {err}"
        );
    }

    #[test]
    fn check_yaml_structure_accepts_block_scalars() {
        let yaml = "users:\n  - name: x\nwrite_files:\n  - path: /etc/example\n    content: |\n      first line\n      second line\nfoo: bar\n";
        check_yaml_structure(yaml).expect("block scalars must validate cleanly");
    }
}
