#!/bin/bash
# tartarus-update-claude.sh — refresh the per-user Claude CLI install.
# Must run as the in-guest user, never root.

set -euo pipefail

INSTALL_PREFIX="${HOME}/.local"
CLAUDE_BIN="${INSTALL_PREFIX}/bin/claude"
PACKAGE_NAME="@anthropic-ai/claude-code"

log() {
    printf '[ tartarus-update-claude ] %s\n' "$*" >&2
}

refuse_root() {
    if [[ "$(id -u)" == "0" ]]; then
        log "refusing to run as root; Claude CLI must be updated as the invoker user"
        exit 1
    fi
}

current_version() {
    if [[ ! -x "${CLAUDE_BIN}" ]]; then
        printf '%s' "(none)"
        return 0
    fi
    "${CLAUDE_BIN}" --version 2>/dev/null | head -n1 || printf '%s' "(unknown)"
}

install_latest() {
    if ! command -v npm >/dev/null 2>&1; then
        log "npm is not on PATH; cannot update Claude CLI"
        return 1
    fi
    mkdir -p "${INSTALL_PREFIX}"
    log "running: npm install --prefix=${INSTALL_PREFIX} ${PACKAGE_NAME}@^1"
    npm install --prefix="${INSTALL_PREFIX}" "${PACKAGE_NAME}@^1"
}

validate_install() {
    if [[ ! -x "${CLAUDE_BIN}" ]]; then
        log "post-install: ${CLAUDE_BIN} is not executable"
        return 1
    fi
    if ! "${CLAUDE_BIN}" --version >/dev/null 2>&1; then
        log "post-install: ${CLAUDE_BIN} --version failed"
        return 1
    fi
}

main() {
    refuse_root
    local before
    before="$(current_version)"
    log "current Claude CLI version: ${before}"

    if install_latest && validate_install; then
        local after
        after="$(current_version)"
        log "Claude CLI updated: ${before} -> ${after}"
    else
        log "update failed; keeping the existing install (${before})"
        exit 1
    fi
}

main "$@"
