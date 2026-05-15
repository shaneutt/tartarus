#!/bin/bash
# tartarus-claude.sh — launch claude agents in the default repo.

set -euo pipefail

WRAPPER="${WRAPPER:-/usr/local/bin/tartarus-env-wrapper.sh}"
WORKDIR_BASE="${WORKDIR_BASE:-${HOME}/tartarus/repositories}"
REPOS_MANIFEST="${REPOS_MANIFEST:-/etc/tartarus/repos}"

log() {
    printf '[ tartarus-claude ] %s\n' "$*" >&2
}

default_repo_dir() {
    if [[ -f "${REPOS_MANIFEST}" ]]; then
        local default_slug=""
        local first_slug=""
        while IFS=$'\t' read -r slug flag; do
            [[ -n "${slug}" ]] || continue
            [[ -z "${first_slug}" ]] && first_slug="${slug}"
            if [[ "${flag:-}" == "default" ]]; then
                default_slug="${slug}"
                break
            fi
        done <"${REPOS_MANIFEST}"
        local picked="${default_slug:-${first_slug}}"
        if [[ -n "${picked}" ]]; then
            local name="${picked#*/}"
            local target="${WORKDIR_BASE}/${name}"
            if [[ -d "${target}" ]]; then
                printf '%s' "${target}"
                return 0
            fi
        fi
    fi
    printf '%s' "${HOME}"
}

main() {
    local cwd
    cwd="$(default_repo_dir)"
    log "starting claude agents in ${cwd}"
    cd "${cwd}"
    exec "${WRAPPER}" claude agents
}

if [[ "${BASH_SOURCE[0]:-$0}" == "${0}" ]]; then
    main "$@"
fi
