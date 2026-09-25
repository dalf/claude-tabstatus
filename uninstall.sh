#!/bin/sh
# claude-tabstatus uninstaller. Puts back exactly what install.sh recorded.
#
#   sh uninstall.sh                    undo the symlink and the env key
#   sh uninstall.sh --force            undo them even with no state record
#   sh uninstall.sh --restore-backup   undo the symlink, then restore
#                                      settings.json from the pre-install copy
#
# <config>/claude-tabstatus.state is install.sh's record of what was there
# before it ran. It is what makes this an undo rather than a delete: if the
# user had already set CLAUDE_CODE_DISABLE_TERMINAL_TITLE themselves, that
# value is restored instead of being thrown away. With no state record, a key
# that is already present is NOT removed without --force, because there is no
# way to tell it apart from the user's own.
#
# CLAUDE_CONFIG_DIR overrides the config directory.

set -eu

die() {
    printf 'error: %s\n' "$1" >&2
    shift
    for _line in "$@"; do printf '       %s\n' "$_line" >&2; done
    exit 1
}

# The jq check comes first so its message is what you see when jq is missing.
command -v jq >/dev/null 2>&1 || die \
    'jq is required and was not found on PATH.' \
    'settings.json is edited with jq so that no other key can be clobbered.' \
    'Install it and re-run.'

# Parameter expansion rather than dirname: one less external to depend on.
case $0 in
*/*) _dir=${0%/*} ;;
*) _dir=. ;;
esac
repo=$(cd -- "$_dir" && pwd)
config_dir=${CLAUDE_CONFIG_DIR:-$HOME/.claude}
link=$config_dir/skills/claude-tabstatus
settings=$config_dir/settings.json
state=$config_dir/claude-tabstatus.state
key=CLAUDE_CODE_DISABLE_TERMINAL_TITLE
settings_link_note=

restore_backup=0
force=0
for arg in "$@"; do
    case $arg in
    --restore-backup) restore_backup=1 ;;
    --force) force=1 ;;
    -h | --help)
        printf 'usage: sh uninstall.sh [--force] [--restore-backup]\n'
        exit 0
        ;;
    *)
        printf 'error: unknown option %s\n' "$arg" >&2
        exit 1
        ;;
    esac
done

# Follow a dotfiles-managed symlink, so the rename below lands on the real
# file instead of replacing the link with a regular file.
if [ -L "$settings" ]; then
    _real=$(readlink -f -- "$settings" 2>/dev/null) || _real=
    [ -n "$_real" ] || die \
        "$settings is a symlink whose target could not be resolved." \
        'Refusing to touch it, and nothing else has been changed.'
    settings_link_note="following the settings.json symlink to $_real"
    settings=$_real
fi
backup=$settings.cctab-preinstall
safety=$settings.cctab-preuninstall

# Preflight every target before changing any of them.
if [ -e "$link" ] && [ ! -L "$link" ]; then
    die "$link exists but is not a symlink." \
        'install.sh did not create it, so it is not ours to remove.' \
        'Refusing to touch it, and nothing else has been changed.'
fi
if [ -e "$settings" ]; then
    jq -e 'type == "object" and ((has("env") | not) or (.env | type == "object"))' \
        "$settings" >/dev/null 2>&1 || die \
        "$settings is not the expected shape" \
        '(a JSON object whose "env", if present, is an object).' \
        'Refusing to touch it, and nothing else has been changed.'
    [ -w "$settings" ] || die "$settings is read-only." \
        'Refusing to overwrite it.'
fi
if [ -e "$state" ]; then
    jq -e . "$state" >/dev/null 2>&1 || die \
        "$state is not valid JSON." \
        'Delete it and re-run with --force if you want to uninstall anyway.'
fi

printf 'config:   %s\n' "$config_dir"
[ -z "$settings_link_note" ] || printf 'settings: %s\n' "$settings_link_note"
printf '\n'

tmp=$settings.cctab-tmp.$$
cleanup() { rm -f "$tmp"; }
# Split: a signal trap does not abort a non-interactive shell, so a combined
# `trap cleanup EXIT HUP INT TERM` would clean up and then carry on.
trap cleanup EXIT
trap 'cleanup; exit 130' HUP INT TERM

# --- what install.sh recorded ----------------------------------------------
have_state=0
env_had=
link_had=
link_target=
if [ -e "$state" ]; then
    have_state=1
    env_had=$(jq -r '.env_key_before.had // false' "$state")
    link_had=$(jq -r '.symlink_before.had // false' "$state")
    link_target=$(jq -r '.symlink_before.target // ""' "$state")
    printf 'state:    %s\n' "$state"
else
    printf 'state:    no record at %s\n' "$state"
fi

# --- 1. the symlink --------------------------------------------------------
if [ -L "$link" ]; then
    current=$(readlink "$link")
    if [ "$have_state" = 1 ] && [ "$link_had" = true ] && [ -n "$link_target" ]; then
        rm -f "$link"
        ln -s "$link_target" "$link"
        printf 'symlink:  put back the target install.sh found here\n'
        printf '          %s -> %s\n' "$link" "$link_target"
    else
        rm -f "$link"
        printf 'symlink:  removed %s\n' "$link"
        printf '          (was -> %s)\n' "$current"
    fi
elif [ -e "$link" ]; then
    die "$link exists but is not a symlink." \
        'install.sh did not create it, so it is not ours to remove.' \
        'Refusing to touch it.'
else
    printf 'symlink:  not present - nothing to remove\n'
fi

# --- 2. settings.json ------------------------------------------------------
if [ "$restore_backup" = 1 ]; then
    # One fixed backup name, so there is no glob, no timestamp sort and no
    # locale-dependent collation to pick the wrong file - and in particular no
    # way to restore this script's own pre-uninstall safety copy.
    [ -e "$backup" ] || die "no pre-install backup at $backup." \
        'install.sh only writes one when it actually changes settings.json.'
    jq -e . "$backup" >/dev/null 2>&1 || die \
        "$backup is not valid JSON." "Refusing to restore it over $settings."

    if [ -e "$settings" ]; then
        cp -p "$settings" "$safety"
        printf 'settings: current file saved to %s\n' "$safety"
    else
        printf 'settings: %s is missing - restoring it from the backup\n' "$settings"
    fi
    cp -p "$backup" "$tmp"
    mv -f "$tmp" "$settings"
    printf 'settings: restored from %s\n' "$backup"
elif [ ! -e "$settings" ]; then
    printf 'settings: %s does not exist - nothing to do\n' "$settings"
else
    present=1
    [ -n "$(jq -r --arg k "$key" '.env[$k] // empty' "$settings")" ] || present=0

    if [ "$present" = 0 ] && [ "$env_had" != true ]; then
        printf 'settings: env.%s is not set - unchanged\n' "$key"
    elif [ "$have_state" = 0 ] && [ "$force" = 0 ]; then
        printf 'settings: env.%s IS set, but there is no record that we set it.\n' "$key" >&2
        printf '          Not removing a key this uninstaller cannot prove it created.\n' >&2
        printf '          Re-run with --force to remove it anyway:\n' >&2
        printf '              sh %s/uninstall.sh --force\n' "$repo" >&2
        printf '\nDone (settings.json left alone).\n'
        exit 0
    else
        # Target value: whatever install.sh found here, or nothing at all.
        if [ "$have_state" = 1 ] && [ "$env_had" = true ]; then
            filter='.env = ((.env // {}) + {($k): $v})
                    | if .env == {} then del(.env) else . end'
            prior=$(jq -c '.env_key_before.value' "$state")
            action=restored
        else
            filter='del(.env[$k]) | if .env == {} then del(.env) else . end'
            prior=null
            action=removed
        fi

        # Nothing to do if the file already holds the recorded value.
        same=0
        if [ "$action" = restored ] &&
            [ "$(jq -c --arg k "$key" '.env[$k]' "$settings")" = "$prior" ]; then
            same=1
        fi
        if [ "$same" = 1 ]; then
            printf 'settings: env.%s already holds the value install.sh found - unchanged\n' "$key"
        else
            cp -p "$settings" "$safety"
            printf 'settings: backed up to %s\n' "$safety"

            # cp -p first, then truncate through the redirection, so $tmp
            # inherits the original's mode and the rename cannot widen a
            # settings.json locked to 0600.
            cp -p "$settings" "$tmp"
            jq --arg k "$key" --argjson v "$prior" "$filter" "$settings" >"$tmp"

            jq -e . "$tmp" >/dev/null 2>&1 || die \
                'the edited file did not parse as JSON.' \
                "$settings is untouched; the backup is at $safety."

            # Clobber guard: with our key stripped from both documents they
            # must be identical, which proves only that key changed.
            norm='del(.env[$k]) | if .env == {} then del(.env) else . end'
            before=$(jq -S --arg k "$key" "$norm" "$settings")
            after=$(jq -S --arg k "$key" "$norm" "$tmp")
            [ "$before" = "$after" ] || die \
                'the edit would have changed something other than our one key.' \
                "$settings is untouched; the backup is at $safety."

            mv -f "$tmp" "$settings"
            if [ "$action" = restored ]; then
                printf 'settings: restored env.%s = %s (the value install.sh found here)\n' \
                    "$key" "$prior"
            else
                printf 'settings: removed env.%s\n' "$key"
                if jq -e 'has("env")' "$settings" >/dev/null 2>&1; then
                    printf '          "env" kept (it still has other keys)\n'
                else
                    printf '          "env" was empty and was removed too\n'
                fi
            fi
        fi
    fi
fi

# --- 3. the state record ---------------------------------------------------
if [ -e "$state" ]; then
    rm -f "$state"
    printf 'state:    removed %s\n' "$state"
fi

printf '\nDone. Start a NEW Claude Code session for the change to take effect.\n'
for b in "$backup" "$safety"; do
    [ -e "$b" ] || continue
    printf 'A copy of settings.json is left at %s - delete it when you are happy.\n' "$b"
done
printf 'The repo itself at %s was not touched.\n' "$repo"
