#!/bin/bash
# tartarus-update.sh — orchestrate a session-wide update (system
# packages, Claude CLI, fstrim).

set -euo pipefail

USER_NAME="${TARTARUS_USER:-${SUDO_USER:-$(cat /etc/tartarus/tartarus-user 2>/dev/null || getent passwd 1000 | cut -d: -f1)}}"
CLAUDE_UPDATER="${CLAUDE_UPDATER:-/usr/local/bin/tartarus-update-claude.sh}"

log() {
    printf '[ tartarus-update ] %s\n' "$*" >&2
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

upgrade_system_packages() {
    log "starting tartarus-update-system.service (dnf upgrade --refresh -y)"
    systemctl start --wait tartarus-update-system.service
    log "system package upgrade complete"
}

upgrade_claude_as_user() {
    log "running Claude CLI updater as user '${USER_NAME}'"
    runuser -u "${USER_NAME}" -- "${CLAUDE_UPDATER}"
    log "Claude CLI update complete"
}

trim_overlay() {
    log "starting tartarus-fstrim.service (fstrim -av)"
    systemctl start --wait tartarus-fstrim.service
    log "fstrim complete"
}

main() {
    require_user
    upgrade_system_packages
    upgrade_claude_as_user
    trim_overlay
    log "update complete"
}

main "$@"
