#!/bin/bash
# tartarus-env-add.sh — idempotent install of a programming environment
# (rust, go, or python). Invoked via qemu-guest-agent from the host.

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

usage() {
    log "usage: $0 <rust|go|python> [--components C1,C2] [--toolchains T1,T2] [--cargo-tools T1,T2]"
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

rust_install() {
    local components="$1"
    local toolchains="$2"
    local cargo_tools="$3"

    log "rust: installing rustup via dnf (root)"
    dnf install -y rustup

    log "rust: running rustup-init as ${USER_NAME}"
    runuser -u "${USER_NAME}" -- bash -lc 'rustup-init -y --no-modify-path --default-toolchain stable'

    if [[ -n "${components}" ]]; then
        local comp
        while IFS= read -r comp; do
            [[ -n "${comp}" ]] || continue
            log "rust: rustup component add ${comp} (default toolchain)"
            runuser -u "${USER_NAME}" -- bash -lc "source \"\${HOME}/.cargo/env\"; rustup component add '${comp}'"
        done < <(split_csv "${components}")
    fi

    if [[ -n "${toolchains}" ]]; then
        local tc
        while IFS= read -r tc; do
            [[ -n "${tc}" ]] || continue
            if [[ "${tc}" == "stable" ]]; then
                continue
            fi
            log "rust: rustup toolchain install ${tc}"
            runuser -u "${USER_NAME}" -- bash -lc "source \"\${HOME}/.cargo/env\"; rustup toolchain install '${tc}'"
        done < <(split_csv "${toolchains}")
    fi

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
    local components="$1"
    local toolchains="$2"
    local cargo_tools="$3"

    if rust_present; then
        log "rust already present; no-op"
        return 0
    fi
    rust_install "${components}" "${toolchains}" "${cargo_tools}"
    log "rust installed"
}

go_present() {
    command -v go >/dev/null 2>&1 && go version >/dev/null 2>&1
}

go_install() {
    log "go: installing golang via dnf (root)"
    dnf install -y golang

    log "go: configuring GOPATH for ${USER_NAME}"
    runuser -u "${USER_NAME}" -- bash -lc 'go env -w GOPATH="${HOME}/go"'

    log "go: ensuring shell rc wires GOPATH/bin into PATH"
    runuser -u "${USER_NAME}" -- bash -lc '
        rc="${HOME}/.bashrc"
        line='\''export PATH="${PATH}:$(go env GOPATH)/bin"'\''
        touch "${rc}"
        if ! grep -F -q "${line}" "${rc}"; then
            printf "\n# tartarus: GOPATH/bin\n%s\n" "${line}" >>"${rc}"
        fi
    '
}

go_main() {
    if go_present; then
        log "go already present; no-op"
        return 0
    fi
    go_install
    log "go installed"
}

python_present() {
    # python3 itself is in every Fedora cloud image, and `python3 -m venv` is
    # a stdlib module that ships with it — so neither is a discriminator for
    # whether the env package set is installed. The python env contract is
    # `python3 + python3-virtualenv + python3-pip`; `python3-virtualenv` is
    # the canonical "is the env present?" probe (pip is bundled by it).
    rpm -q python3-virtualenv >/dev/null 2>&1
}

python_install() {
    log "python: installing python3 + virtualenv + pip via dnf (root)"
    dnf install -y python3 python3-virtualenv python3-pip
}

python_main() {
    if python_present; then
        log "python already present; no-op"
        return 0
    fi
    python_install
    log "python installed"
}

parse_args() {
    if [[ $# -lt 1 ]]; then
        usage
        exit 2
    fi
    ENV_NAME="$1"
    shift

    COMPONENTS=""
    TOOLCHAINS=""
    CARGO_TOOLS=""

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --components)
                COMPONENTS="${2:-}"
                shift 2
                ;;
            --toolchains)
                TOOLCHAINS="${2:-}"
                shift 2
                ;;
            --cargo-tools)
                CARGO_TOOLS="${2:-}"
                shift 2
                ;;
            *)
                log "unknown argument: $1"
                usage
                exit 2
                ;;
        esac
    done
}

main() {
    parse_args "$@"
    require_user

    case "${ENV_NAME}" in
        rust)
            rust_main "${COMPONENTS}" "${TOOLCHAINS}" "${CARGO_TOOLS}"
            ;;
        go)
            go_main
            ;;
        python)
            python_main
            ;;
        *)
            log "unknown env '${ENV_NAME}'; expected one of rust|go|python"
            exit 2
            ;;
    esac
}

main "$@"
