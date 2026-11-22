#!/usr/bin/env bash
# Update Lynapse: get the latest version from GitHub and reinstall it.
#
#   ./update.sh            update if there is something new
#   ./update.sh --force    rebuild and reinstall even if already up to date
#
# Keeps your settings, your autostart choice and your Spatial Audio filter (including one you built with
# dsp/synapse/build.py). Extra options are passed to install.sh (see ./install.sh --help).
set -euo pipefail
cd "$(dirname "$0")"

FORCE=0
ARGS=()
for a in "$@"; do
    case "$a" in
        --force) FORCE=1 ;;
        -h|--help) sed -n '2,9p' "$0"; exit 0 ;;
        *) ARGS+=("$a") ;;
    esac
done

say() { printf '\n\033[1;32m==> %s\033[0m\n' "$*"; }

[[ -d .git ]] || { echo "This folder is not a git clone of Lynapse2, so it can't update itself."; echo "Clone it again: git clone https://github.com/Stealthee/Lynapse2.git"; exit 1; }
if ! git diff --quiet || ! git diff --cached --quiet; then
    echo "You have changed files in this folder (git status shows them). Commit or stash them first:"
    echo "  git stash && ./update.sh && git stash pop"
    exit 1
fi

say "Getting the latest version"
git pull --ff-only

INSTALLED=$(sed -n 's/.*"commit": *"\([0-9a-f]*\)".*/\1/p' "$HOME/.config/razerctl/install.json" 2>/dev/null || true)
if [[ $FORCE == 0 && -n "$INSTALLED" && "$INSTALLED" == "$(git rev-parse HEAD)" && -x "$HOME/.local/bin/razerctl" ]]; then
    echo "Lynapse is already up to date ($(sed -n 's/^version = "\(.*\)"/\1/p' src-tauri/Cargo.toml)). Use --force to reinstall anyway."
    exit 0
fi

# Reinstall the app only: --no-spatial leaves an existing Spatial Audio filter exactly as it is.
OPTS=(--no-spatial)
[[ -f "$HOME/.config/autostart/razerctl.desktop" ]] || OPTS+=(--no-autostart)
exec ./install.sh "${OPTS[@]}" "${ARGS[@]}"
