#!/bin/bash
# tartarus-env-update.sh — update installed programming environments
# (rust, go, python). Skips envs that are not installed.

set -euo pipefail

USER_NAME="${TARTARUS_USER:-${SUDO_USER:-$(cat /etc/tartarus/tartarus-user 2>/dev/null || getent passwd 1000 | cut -d: -f1)}}"

log() {
    printf '[ tartarus-env ] %s\n' "$*" >&2
}

require_user() {
    if [[ -z "${USER_NAME}" ]]; then
        log "could not determine the in-guest user (set TARTARUS_USER explicitly)"
        exit 1
    fi
    if ! id -u "${USER_NAME}" >/dev/null 2>&1; then
        log "user '${USER_NAME}' does not exist in /etc/passwd"
        exit 1
    fi
}

split_csv() {
    local raw="${1:-}"
    if [[ -z "${raw}" ]]; then
        return 0
    fi
    local IFS=','
    # shellcheck disable=SC2206
    local parts=( ${raw} )
    printf '%s\n' "${parts[@]}"
}

rust_present() {
    runuser -u "${USER_NAME}" -- bash -lc 'command -v rustup >/dev/null 2>&1 && rustup --version >/dev/null 2>&1'
}

rust_update() {
    local cargo_tools="$1"

    log "rust: rustup update --no-self-update (as ${USER_NAME})"
    runuser -u "${USER_NAME}" -- bash -lc 'source "${HOME}/.cargo/env"; rustup update --no-self-update'

    if [[ -n "${cargo_tools}" ]]; then
        local tool
        while IFS= read -r tool; do
            [[ -n "${tool}" ]] || continue
            log "rust: cargo install --locked ${tool}"
            runuser -u "${USER_NAME}" -- bash -lc "source \"\${HOME}/.cargo/env\"; cargo install --locked '${tool}'"
        done < <(split_csv "${cargo_tools}")
    fi
}

rust_main() {
    local cargo_tools="$1"

    if ! rust_present; then
        log "rust not installed; skipping"
        return 0
    fi
    rust_update "${cargo_tools}"
    log "rust update complete"
}

go_present() {
    command -v go >/dev/null 2>&1 && go version >/dev/null 2>&1
}

go_update() {
    log "go: dnf upgrade -y golang (root)"
    dnf upgrade -y golang
}

go_main() {
    if ! go_present; then
        log "go not installed; skipping"
        return 0
    fi
    go_update
    log "go update complete"
}

python_present() {
    # python3 itself is in every Fedora cloud image, and `python3 -m venv` is
    # a stdlib module that ships with it — so neither is a discriminator for
    # whether the env package set is installed. The python env contract is
    # `python3 + python3-virtualenv + python3-pip`; `python3-virtualenv` is
    # the canonical "is the env present?" probe (pip is bundled by it).
    rpm -q python3-virtualenv >/dev/null 2>&1
}

python_update() {
    log "python: dnf upgrade -y python3 python3-virtualenv python3-pip (root)"
    dnf upgrade -y python3 python3-virtualenv python3-pip
}

python_main() {
    if ! python_present; then
        log "python not installed; skipping"
        return 0
    fi
    python_update
    log "python update complete"
}

parse_args() {
    CARGO_TOOLS=""

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --cargo-tools)
                CARGO_TOOLS="${2:-}"
                shift 2
                ;;
            *)
                log "unknown argument: $1"
                exit 2
                ;;
        esac
    done
}

main() {
    parse_args "$@"
    require_user

    rust_main "${CARGO_TOOLS}"
    go_main
    python_main

    log "env update complete"
}

main "$@"
