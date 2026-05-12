#!/bin/bash
# tartarus-env-wrapper.sh — source /run/tartarus/env/* then exec argv.

set -euo pipefail

ENV_DIR="${ENV_DIR:-/run/tartarus/env}"

if [[ -d "${ENV_DIR}" ]]; then
    set -a
    for f in "${ENV_DIR}"/*; do
        [[ -f "${f}" ]] || continue
        # shellcheck disable=SC1090
        source "${f}"
    done
    set +a
fi

if [[ $# -eq 0 ]]; then
    printf 'tartarus-env-wrapper: no command supplied\n' >&2
    exit 64
fi

exec "$@"
