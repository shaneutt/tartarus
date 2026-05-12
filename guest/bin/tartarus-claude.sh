#!/bin/bash
# tartarus-claude.sh — launch tmux + claude on tty1 in the default repo.

set -euo pipefail

WRAPPER="${WRAPPER:-/usr/local/bin/tartarus-env-wrapper.sh}"
WORKDIR_BASE="${WORKDIR_BASE:-${HOME}/tartarus/repositories}"
REPOS_MANIFEST="${REPOS_MANIFEST:-/etc/tartarus/repos}"
TMUX_SESSION="${TMUX_SESSION:-work}"

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
    log "starting tmux session '${TMUX_SESSION}' cd'd to ${cwd}"
    exec tmux new-session -As "${TMUX_SESSION}" -c "${cwd}" "${WRAPPER}" claude
}

if [[ "${BASH_SOURCE[0]:-$0}" == "${0}" ]]; then
    main "$@"
fi
