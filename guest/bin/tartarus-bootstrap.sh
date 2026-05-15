#!/bin/bash
# tartarus-bootstrap.sh — first-boot bootstrap (gh auth, repo clone,
# claude trigger). Runs as the in-guest user via a seed-injected
# systemd drop-in; see tartarus-bootstrap.service for details.

set -euo pipefail

ENV_DIR="${ENV_DIR:-/run/tartarus/env}"
REPOS_MANIFEST="${REPOS_MANIFEST:-/etc/tartarus/repos}"
WORKDIR_BASE="${WORKDIR_BASE:-${HOME}/tartarus/repositories}"
CLAUDE_TARBALL="${CLAUDE_TARBALL:-/opt/tartarus/skeleton/claude-code.tgz}"

log() {
    printf '[ tartarus-bootstrap ] %s\n' "$*" >&2
}

source_env_dir() {
    if [[ ! -d "${ENV_DIR}" ]]; then
        log "no env dir at ${ENV_DIR}; nothing to source"
        return 0
    fi

    set -a
    for f in "${ENV_DIR}"/*; do
        [[ -f "${f}" ]] || continue
        # shellcheck disable=SC1090
        source "${f}"
    done

    set +a
}

require_github_token() {
    if [[ -z "${GITHUB_TOKEN:-}" ]]; then
        log "GITHUB_TOKEN not set; cannot authenticate gh"
        exit 1
    fi
}

authenticate_gh() {
    require_github_token
    log "authenticating gh from \$GITHUB_TOKEN"
    gh auth login --with-token <<<"${GITHUB_TOKEN}"
}

clone_repos() {
    if [[ ! -f "${REPOS_MANIFEST}" ]]; then
        log "no repo manifest at ${REPOS_MANIFEST}; skipping clone"
        return 0
    fi
    mkdir -p "${WORKDIR_BASE}"
    while IFS=$'\t' read -r slug _flag; do
        [[ -n "${slug}" ]] || continue
        local name
        name="${slug#*/}"
        local target="${WORKDIR_BASE}/${name}"
        if [[ -d "${target}/.git" ]]; then
            log "${slug}: clone target ${target} already exists; skipping"
            continue
        fi

        log "cloning ${slug} -> ${target}"
        git clone --depth 1 "https://github.com/${slug}.git" "${target}"
    done <"${REPOS_MANIFEST}"
}

start_claude_service() {
    local user="${USER:-$(id -un)}"
    log "triggering tartarus-claude@${user}.service"
    systemctl start "tartarus-claude@${user}.service"
}

install_claude_if_missing() {
    if [[ -x "${HOME}/.local/bin/claude" ]]; then
        log "claude already installed at ${HOME}/.local/bin/claude; skipping install"
        return 0
    fi
    if [[ ! -f "${CLAUDE_TARBALL}" ]]; then
        log "no Claude tarball at ${CLAUDE_TARBALL}; skipping install"
        return 0
    fi

    log "installing Claude from ${CLAUDE_TARBALL} into ${HOME}/.local"
    mkdir -p "${HOME}/.local"
    npm install --prefix="${HOME}/.local" "${CLAUDE_TARBALL}"
}

main() {
    source_env_dir
    authenticate_gh
    if [[ "${TARTARUS_SKIP_CLAUDE:-}" != "1" ]]; then
        install_claude_if_missing
    fi
    clone_repos
    if [[ "${TARTARUS_SKIP_CLAUDE:-}" != "1" ]]; then
        start_claude_service
    fi
    log "bootstrap complete"
}

main "$@"
