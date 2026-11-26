#!/bin/bash
set -u

parent=$1
target=$2
stage=$3
version=$4
executable="$target/Contents/MacOS/pordo"
backup="$stage/previous.app"
replacement="$stage/Pordo.app"

cleanup() {
    # A failed restoration must leave the old bundle available for recovery.
    if [[ ! -e "$backup" ]]; then
        /bin/rm -rf "$stage"
    fi
}
trap cleanup EXIT

relaunch_old() {
    PORDO_UPDATE_ROLLBACK=1 "$executable" </dev/null >/dev/null 2>&1 &
}

rollback() {
    if [[ -e "$target" ]] && ! /bin/mv "$target" "$stage/failed.app"; then
        exit 1
    fi
    /bin/mv "$backup" "$target" || exit 1
    relaunch_old
    exit 1
}

deadline=$((SECONDS + 30))
while kill -0 "$parent" 2>/dev/null; do
    if (( SECONDS >= deadline )); then
        echo 'The current application did not exit.' >&2
        exit 1
    fi
    /bin/sleep 0.1
done

if ! processes=$(/bin/ps -ww -axo 'pid=,comm='); then
    relaunch_old
    exit 1
fi
while read -r pid command; do
    if [[ "$command" == "$executable" ]]; then
        echo 'Another window of this installation is still running.' >&2
        exit 1
    fi
done <<< "$processes"

if ! /bin/mv "$target" "$backup"; then
    relaunch_old
    exit 1
fi
/bin/mv "$replacement" "$target" || rollback

PORDO_UPDATE_VERSION="$version" PORDO_UPDATE_STAGING="$stage" \
    "$executable" </dev/null >/dev/null 2>&1 &
replacement_pid=$!
deadline=$((SECONDS + 30))
while (( SECONDS < deadline )); do
    if [[ -f "$stage/healthy" ]] && kill -0 "$replacement_pid" 2>/dev/null; then
        /bin/rm -rf "$backup"
        exit 0
    fi
    if ! kill -0 "$replacement_pid" 2>/dev/null; then
        break
    fi
    /bin/sleep 0.1
done
echo 'The replacement application did not confirm startup.' >&2
kill -KILL "$replacement_pid" 2>/dev/null || true
wait "$replacement_pid" 2>/dev/null || true
rollback
