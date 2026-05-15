#!/bin/bash
# tartarus-ferry.sh — copy git changes between GitHub accounts.
#
# Designed for the "ferry VM" workflow: Claude works on a repo
# under a secondary GitHub account, then this script mirrors
# changes to the primary account's repo.
#
# Prerequisites:
#   - gh CLI authenticated with both accounts
#     (gh auth login for each, switch with gh auth switch)
#
# Usage:
#   tartarus-ferry init   --source owner/repo --dest owner/repo \
#                          --source-account user1 --dest-account user2
#   tartarus-ferry sync   [--branch name] [--dry-run]
#   tartarus-ferry status

set -euo pipefail

FERRY_CONF="${FERRY_CONF:-${HOME}/.config/tartarus/ferry.conf}"
FERRY_RELAY_DIR="${FERRY_RELAY_DIR:-${HOME}/.local/share/tartarus/ferry}"

log() { printf '[ tartarus-ferry ] %s\n' "$*" >&2; }

usage() {
    cat >&2 <<'EOF'
Usage:
  tartarus-ferry init --source OWNER/REPO --dest OWNER/REPO \
                      --source-account USER --dest-account USER
  tartarus-ferry sync [--branch BRANCH] [--dry-run]
  tartarus-ferry status
EOF
}

# ---------------------------------------------------------------------------
# Config
# ---------------------------------------------------------------------------

load_config() {
    if [[ ! -f "${FERRY_CONF}" ]]; then
        log "no config at ${FERRY_CONF}; run 'tartarus-ferry init' first"
        exit 1
    fi
    # shellcheck disable=SC1090
    source "${FERRY_CONF}"
    for var in SOURCE_REPO DEST_REPO SOURCE_ACCOUNT DEST_ACCOUNT; do
        if [[ -z "${!var:-}" ]]; then
            log "config missing ${var}; re-run 'tartarus-ferry init'"
            exit 1
        fi
    done
}

save_config() {
    mkdir -p "$(dirname "${FERRY_CONF}")"
    cat > "${FERRY_CONF}" <<EOF
SOURCE_REPO=${SOURCE_REPO}
DEST_REPO=${DEST_REPO}
SOURCE_ACCOUNT=${SOURCE_ACCOUNT}
DEST_ACCOUNT=${DEST_ACCOUNT}
EOF
    chmod 0600 "${FERRY_CONF}"
    log "config written to ${FERRY_CONF}"
}

# ---------------------------------------------------------------------------
# Token helpers
# ---------------------------------------------------------------------------

get_token() {
    local account="$1"
    local token
    token="$(gh auth token --user "${account}" 2>/dev/null)" || true
    if [[ -z "${token}" ]]; then
        log "no gh token for account '${account}'"
        log "hint: run 'gh auth login' to authenticate this account"
        exit 1
    fi
    printf '%s' "${token}"
}

credential_helper_for() {
    local token="$1"
    printf '!f() { echo username=x-access-token; echo "password=%s"; }; f' "${token}"
}

# ---------------------------------------------------------------------------
# init
# ---------------------------------------------------------------------------

cmd_init() {
    local source_repo="" dest_repo="" source_account="" dest_account=""

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --source)       source_repo="$2"; shift 2 ;;
            --dest)         dest_repo="$2"; shift 2 ;;
            --source-account) source_account="$2"; shift 2 ;;
            --dest-account)   dest_account="$2"; shift 2 ;;
            *) log "unknown flag: $1"; usage; exit 1 ;;
        esac
    done

    if [[ -z "${source_repo}" || -z "${dest_repo}" ||
          -z "${source_account}" || -z "${dest_account}" ]]; then
        log "all four flags are required"
        usage
        exit 1
    fi

    for account in "${source_account}" "${dest_account}"; do
        if ! gh auth token --user "${account}" &>/dev/null; then
            log "account '${account}' is not authenticated in gh"
            log "hint: run 'gh auth login' and log in as ${account}"
            exit 1
        fi
    done

    SOURCE_REPO="${source_repo}"
    DEST_REPO="${dest_repo}"
    SOURCE_ACCOUNT="${source_account}"
    DEST_ACCOUNT="${dest_account}"
    save_config

    local repo_name="${SOURCE_REPO#*/}"
    local relay="${FERRY_RELAY_DIR}/${repo_name}.git"

    if [[ -d "${relay}" ]]; then
        log "relay repo already exists at ${relay}"
    else
        log "creating bare relay clone of ${SOURCE_REPO}"
        local src_token
        src_token="$(get_token "${SOURCE_ACCOUNT}")"
        mkdir -p "${FERRY_RELAY_DIR}"
        git -c credential.helper="$(credential_helper_for "${src_token}")" \
            clone --bare "https://github.com/${SOURCE_REPO}.git" "${relay}"

        local dest_token
        dest_token="$(get_token "${DEST_ACCOUNT}")"
        git -C "${relay}" remote add dest "https://github.com/${DEST_REPO}.git"
        log "relay ready at ${relay}"
    fi

    log "init complete"
}

# ---------------------------------------------------------------------------
# sync
# ---------------------------------------------------------------------------

cmd_sync() {
    local branch="" dry_run=0

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --branch)  branch="$2"; shift 2 ;;
            --dry-run) dry_run=1; shift ;;
            *) log "unknown flag: $1"; usage; exit 1 ;;
        esac
    done

    load_config

    local repo_name="${SOURCE_REPO#*/}"
    local relay="${FERRY_RELAY_DIR}/${repo_name}.git"

    if [[ ! -d "${relay}" ]]; then
        log "no relay at ${relay}; run 'tartarus-ferry init' first"
        exit 1
    fi

    local src_token dest_token
    src_token="$(get_token "${SOURCE_ACCOUNT}")"
    dest_token="$(get_token "${DEST_ACCOUNT}")"

    local src_helper dest_helper
    src_helper="$(credential_helper_for "${src_token}")"
    dest_helper="$(credential_helper_for "${dest_token}")"

    if [[ "${dry_run}" == "1" ]]; then
        log "[dry-run] fetching from ${SOURCE_REPO} (${SOURCE_ACCOUNT})"
        if [[ -n "${branch}" ]]; then
            git -C "${relay}" -c credential.helper="${src_helper}" \
                fetch origin "${branch}" --dry-run 2>&1 | sed 's/^/  /'
        else
            git -C "${relay}" -c credential.helper="${src_helper}" \
                fetch origin --prune --dry-run 2>&1 | sed 's/^/  /'
        fi
        log "[dry-run] pushing to ${DEST_REPO} (${DEST_ACCOUNT})"
        if [[ -n "${branch}" ]]; then
            git -C "${relay}" -c credential.helper="${dest_helper}" \
                push dest "refs/heads/${branch}:refs/heads/${branch}" \
                --dry-run 2>&1 | sed 's/^/  /'
        else
            git -C "${relay}" -c credential.helper="${dest_helper}" \
                push dest --all --prune --dry-run 2>&1 | sed 's/^/  /'
            git -C "${relay}" -c credential.helper="${dest_helper}" \
                push dest --tags --dry-run 2>&1 | sed 's/^/  /'
        fi
        return
    fi

    log "fetching from ${SOURCE_REPO} (${SOURCE_ACCOUNT})"
    if [[ -n "${branch}" ]]; then
        git -C "${relay}" -c credential.helper="${src_helper}" \
            fetch origin "${branch}"
    else
        git -C "${relay}" -c credential.helper="${src_helper}" \
            fetch origin --prune
    fi

    log "pushing to ${DEST_REPO} (${DEST_ACCOUNT})"
    if [[ -n "${branch}" ]]; then
        git -C "${relay}" -c credential.helper="${dest_helper}" \
            push dest "refs/heads/${branch}:refs/heads/${branch}"
    else
        git -C "${relay}" -c credential.helper="${dest_helper}" \
            push dest --all --prune
        git -C "${relay}" -c credential.helper="${dest_helper}" \
            push dest --tags
    fi

    log "sync complete"
}

# ---------------------------------------------------------------------------
# status
# ---------------------------------------------------------------------------

cmd_status() {
    if [[ ! -f "${FERRY_CONF}" ]]; then
        log "not configured; run 'tartarus-ferry init' first"
        exit 0
    fi

    load_config

    printf 'source:       %s (account: %s)\n' "${SOURCE_REPO}" "${SOURCE_ACCOUNT}"
    printf 'destination:  %s (account: %s)\n' "${DEST_REPO}" "${DEST_ACCOUNT}"

    local repo_name="${SOURCE_REPO#*/}"
    local relay="${FERRY_RELAY_DIR}/${repo_name}.git"

    if [[ -d "${relay}" ]]; then
        printf 'relay:        %s\n' "${relay}"
    else
        printf 'relay:        (not created)\n'
    fi
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

main() {
    local cmd="${1:-help}"
    shift || true
    case "${cmd}" in
        init)   cmd_init "$@" ;;
        sync)   cmd_sync "$@" ;;
        status) cmd_status "$@" ;;
        help|--help|-h) usage ;;
        *) log "unknown command: ${cmd}"; usage; exit 1 ;;
    esac
}

if [[ "${BASH_SOURCE[0]:-$0}" == "${0}" ]]; then
    main "$@"
fi
