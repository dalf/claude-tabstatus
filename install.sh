#!/bin/sh
# claude-tabstatus installer. Idempotent: safe to re-run.
#
# It changes exactly three things:
#   1. one key     <config>/settings.json  ->  env.CLAUDE_CODE_DISABLE_TERMINAL_TITLE = "1"
#   2. a symlink   <config>/skills/claude-tabstatus -> this repo
#   3. a record    <config>/claude-tabstatus.state  -> what was there before
#
# The env key is not optional: without it Claude Code repaints its own terminal
# title every 960ms, right over ours. A plugin cannot set environment
# variables, so it has to go in settings.json.
#
# The state file exists so that uninstall.sh can put back exactly what was
# there rather than deleting a key it cannot prove it created. It is written
# once, on the first install, and removed by uninstall.sh.
#
# settings.json is written FIRST and the symlink LAST: a failure while editing
# settings must not leave the plugin loaded with the built-in title still
# repainting over it, which looks broken rather than uninstalled.
#
# CLAUDE_CONFIG_DIR overrides the config directory, which is how the test
# harness points this at a throwaway copy instead of your real config.

set -eu

die() {
    printf 'error: %s\n' "$1" >&2
    shift
    for _line in "$@"; do printf '       %s\n' "$_line" >&2; done
    exit 1
}

# --- preconditions, before anything is touched -----------------------------
# The jq check comes first so its message is what you see when jq is missing.
command -v jq >/dev/null 2>&1 || die \
    'jq is required and was not found on PATH.' \
    'settings.json is merged with jq so that no existing key can be clobbered.' \
    'Install it (Fedora: sudo dnf install jq) and re-run.'

# Parameter expansion rather than dirname: one less external to depend on.
case $0 in
*/*) _dir=${0%/*} ;;
*) _dir=. ;;
esac
repo=$(cd -- "$_dir" && pwd)
config_dir=${CLAUDE_CONFIG_DIR:-$HOME/.claude}
skills_dir=$config_dir/skills
link=$skills_dir/claude-tabstatus
settings=$config_dir/settings.json
state=$config_dir/claude-tabstatus.state
key=CLAUDE_CODE_DISABLE_TERMINAL_TITLE
settings_link_note=

for f in .claude-plugin/plugin.json hooks/hooks.json scripts/tabstatus.sh; do
    [ -f "$repo/$f" ] || die \
        "$repo does not look like the claude-tabstatus repo (missing $f)."
done

# A dotfiles-managed settings.json is often a symlink. Follow it, so that the
# tmp-file + rename below lands on the real file instead of replacing the link
# with a regular file and quietly orphaning the managed copy.
if [ -L "$settings" ]; then
    _real=$(readlink -f -- "$settings" 2>/dev/null) || _real=
    [ -n "$_real" ] || die \
        "$settings is a symlink whose target could not be resolved." \
        'Refusing to touch it, and nothing else has been changed.'
    settings_link_note="following the settings.json symlink to $_real"
    settings=$_real
fi
case $settings in
*/*) settings_dir=${settings%/*} ;;
*) settings_dir=. ;;
esac

# Preflight every target before changing any of them, so that a refusal leaves
# the machine exactly as it was rather than half installed.
if [ -e "$link" ] && [ ! -L "$link" ]; then
    die "$link already exists and is not a symlink." \
        'Refusing to touch it. Move or remove it yourself, then re-run.'
fi
if [ -e "$config_dir" ]; then
    [ -d "$config_dir" ] || die "$config_dir exists and is not a directory."
    [ -w "$config_dir" ] || die "$config_dir is not writable." \
        'Nothing has been changed.'
fi
if [ -e "$settings" ]; then
    # Shape, not just parseability: a top-level array or a non-object "env"
    # is valid JSON that would make the merge below fail halfway with a raw
    # jq error in the system locale.
    jq -e 'type == "object" and ((has("env") | not) or (.env | type == "object"))' \
        "$settings" >/dev/null 2>&1 || die \
        "$settings is not the expected shape" \
        '(a JSON object whose "env", if present, is an object).' \
        'Refusing to touch it, and nothing else has been changed.'
    [ -w "$settings" ] || die "$settings is read-only." \
        'Refusing to overwrite it. chmod it yourself if that was not deliberate.'
    [ -w "$settings_dir" ] || die "$settings_dir is not writable." \
        'settings.json is replaced through a temporary file in its own' \
        'directory, so that directory has to be writable. Nothing was changed.'
fi

printf 'repo:     %s\n' "$repo"
printf 'config:   %s\n' "$config_dir"
[ -z "$settings_link_note" ] || printf 'settings: %s\n' "$settings_link_note"
printf '\n'

mkdir -p "$config_dir"

tmp=$settings.cctab-tmp.$$
state_tmp=$state.cctab-tmp.$$
backup=$settings.cctab-preinstall
cleanup() { rm -f "$tmp" "$state_tmp"; }
# Split, because a signal trap does not abort a non-interactive shell: with
# `trap cleanup EXIT HUP INT TERM`, a Ctrl-C mid-install would delete the
# temporary files and then carry on through the rest of the file without them.
trap cleanup EXIT
trap 'cleanup; exit 130' HUP INT TERM

# --- 1. record what was here before ----------------------------------------
# Written ONCE. A re-run must not overwrite the first run's record with the
# state this installer itself produced, or uninstall.sh would conclude that the
# env key had always been there.
if [ -e "$state" ]; then
    printf 'state:    %s exists - keeping the original record\n' "$state"
else
    if [ -e "$settings" ]; then
        _env_before=$(jq --arg k "$key" \
            '{had: ((.env | type) == "object" and (.env | has($k))),
              value: (if (.env | type) == "object" then .env[$k] else null end)}' \
            "$settings")
    else
        _env_before='{"had":false,"value":null}'
    fi
    if [ -L "$link" ]; then
        _link_before=$(jq -n --arg t "$(readlink "$link")" '{had:true,target:$t}')
    else
        _link_before='{"had":false,"target":null}'
    fi
    jq -n --arg repo "$repo" --arg settings "$settings" --arg key "$key" \
        --argjson env_before "$_env_before" --argjson link_before "$_link_before" \
        '{state_version: 1,
          written_by: "claude-tabstatus install.sh",
          repo: $repo,
          settings_path: $settings,
          env_key: $key,
          env_key_before: $env_before,
          symlink_before: $link_before}' >"$state_tmp"
    mv -f "$state_tmp" "$state"
    printf 'state:    recorded the prior state in %s\n' "$state"
fi

# --- 2. the settings key ---------------------------------------------------
# Cheap identity of the file we are about to read-modify-write, so a write by
# another live Claude Code session is caught instead of silently lost.
fingerprint() { stat -c '%i %Y %s' "$1" 2>/dev/null || printf 'unavailable\n'; }

if [ ! -e "$settings" ]; then
    # 0600: this is the file that holds the "env" block, which is where people
    # keep API keys.
    (umask 077 && : >"$tmp")
    printf '{\n  "env": {\n    "%s": "1"\n  }\n}\n' "$key" >"$tmp"
    jq -e . "$tmp" >/dev/null 2>&1 || die 'could not write a valid settings.json.'
    mv -f "$tmp" "$settings"
    printf 'settings: created %s (mode 600)\n' "$settings"
    printf '          env.%s = "1"\n' "$key"
elif [ "$(jq -r --arg k "$key" '.env[$k] // empty' "$settings")" = "1" ]; then
    printf 'settings: env.%s is already "1" - unchanged\n' "$key"
    printf '          (no backup was written, because nothing was changed)\n'
else
    stamp=$(fingerprint "$settings")

    # Back up BEFORE touching anything. One fixed name rather than a timestamp
    # series: this installer touches exactly one key, so a history of backups
    # has no value, and a glob over timestamps is what made --restore-backup
    # pick the wrong file.
    cp -p "$settings" "$backup"
    printf 'settings: backed up to %s\n' "$backup"

    # `cp -p` first, then truncate through the redirection: $tmp inherits the
    # original's mode, so the rename below cannot widen a settings.json the
    # user deliberately locked to 0600, and there is no window in which the
    # merged content sits in a world-readable file.
    cp -p "$settings" "$tmp"
    # Merge ONLY our one key. Every other top-level key and every other env
    # key is carried through untouched.
    jq --arg k "$key" '.env = ((.env // {}) + {($k): "1"})' "$settings" >"$tmp"

    jq -e . "$tmp" >/dev/null 2>&1 || die \
        'the merged file did not parse as JSON.' \
        "$settings is untouched; the backup is at $backup."

    # Clobber guard: strip our key from both documents and require them to be
    # identical. That proves the merge added exactly one key and changed
    # nothing else.
    norm='del(.env[$k]) | if .env == {} then del(.env) else . end'
    before=$(jq -S --arg k "$key" "$norm" "$settings")
    after=$(jq -S --arg k "$key" "$norm" "$tmp")
    [ "$before" = "$after" ] || die \
        'the merge would have changed something other than our one key.' \
        "$settings is untouched; the backup is at $backup."

    # jq reprints the whole document, so a hand-formatted file comes back
    # reindented. Nothing is lost, but say so rather than let a whole-file
    # diff be a surprise.
    reflow=
    [ "$(jq . "$settings")" = "$(cat "$settings")" ] || reflow=1

    [ "$(fingerprint "$settings")" = "$stamp" ] || die \
        'settings.json changed while this installer was running' \
        '(another Claude Code session writing it, most likely).' \
        "Nothing was changed; the backup is at $backup. Close that session and re-run."

    mv -f "$tmp" "$settings"
    printf 'settings: set env.%s = "1"\n' "$key"
    printf '          (%s other top-level key(s) preserved)\n' \
        "$(jq -r '[keys[] | select(. != "env")] | length' "$settings")"
    if [ -n "$reflow" ]; then
        printf '          note: the file was reindented to 2-space JSON;\n'
        printf '          the original formatting is preserved in %s\n' "$backup"
    fi
fi

# --- 3. the symlink --------------------------------------------------------
# A plugin directory under <config>/skills/<name>/ auto-loads as long as it has
# .claude-plugin/plugin.json; no marketplace and no enabledPlugins entry are
# needed. It may be a symlink, which is the whole update story: git pull, then
# start a new session.
mkdir -p "$skills_dir"

if [ -L "$link" ]; then
    current=$(readlink "$link")
    if [ "$current" = "$repo" ]; then
        printf 'symlink:  already correct\n'
        printf '          %s -> %s\n' "$link" "$repo"
    else
        rm -f "$link"
        ln -s "$repo" "$link"
        printf 'symlink:  WARNING - repointed a symlink this installer did not create\n'
        printf '          %s\n' "$link"
        printf '          was  %s\n' "$current"
        printf '          now  %s\n' "$repo"
        printf '          uninstall.sh will put the old target back.\n'
    fi
elif [ -e "$link" ]; then
    die "$link already exists and is not a symlink." \
        'Refusing to touch it. Move or remove it yourself, then re-run.'
else
    ln -s "$repo" "$link"
    printf 'symlink:  created\n'
    printf '          %s -> %s\n' "$link" "$repo"
fi

printf '\nDone. Nothing else on this machine was modified.\n'
printf 'Start a NEW Claude Code session for the plugin and the env key to take effect.\n'
printf 'To undo: sh %s/uninstall.sh\n' "$repo"
