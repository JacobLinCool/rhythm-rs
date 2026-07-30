#!/bin/sh
set -eu

repository="JacobLinCool/rhythm-rs"
release_tag="latest"

say() {
    printf '%s\n' "taiko-install: $*"
}

fail() {
    printf '%s\n' "taiko-install: error: $*" >&2
    exit 1
}

require_command() {
    command -v "$1" >/dev/null 2>&1 || fail "required command not found: $1"
}

for command_name in awk chmod curl install mkdir mktemp mv rm tar uname; do
    require_command "$command_name"
done

system_name=$(uname -s)
machine_name=$(uname -m)

case "${system_name}:${machine_name}" in
    Darwin:arm64 | Darwin:aarch64)
        target="aarch64-apple-darwin"
        hash_command="shasum"
        require_command "$hash_command"
        ;;
    Darwin:x86_64)
        target="x86_64-apple-darwin"
        hash_command="shasum"
        require_command "$hash_command"
        ;;
    Linux:x86_64 | Linux:amd64)
        target="x86_64-unknown-linux-gnu"
        hash_command="sha256sum"
        require_command "$hash_command"
        ;;
    *)
        fail "unsupported platform: ${system_name} ${machine_name}"
        ;;
esac

archive_name="taiko-${target}.tar.gz"
release_base_url="https://github.com/${repository}/releases/download/${release_tag}"
install_directory=${TAIKO_INSTALL_DIR:-"${HOME:?HOME must be set when TAIKO_INSTALL_DIR is unset}/.local/bin"}

temporary_directory=$(mktemp -d "${TMPDIR:-/tmp}/taiko-install.XXXXXX")
staged_destination=""

cleanup() {
    if [ -n "$staged_destination" ]; then
        rm -f "$staged_destination"
    fi
    rm -rf "$temporary_directory"
}

trap cleanup 0
trap 'exit 129' 1
trap 'exit 130' 2
trap 'exit 143' 15

archive_path="${temporary_directory}/${archive_name}"
checksums_path="${temporary_directory}/SHA256SUMS"
extract_directory="${temporary_directory}/extract"

say "downloading ${archive_name}"
curl --proto '=https' --tlsv1.2 --fail --silent --show-error --location \
    --output "$archive_path" "${release_base_url}/${archive_name}"
curl --proto '=https' --tlsv1.2 --fail --silent --show-error --location \
    --output "$checksums_path" "${release_base_url}/SHA256SUMS"

checksum_count=$(awk -v name="$archive_name" '$2 == name { count += 1 } END { print count + 0 }' "$checksums_path")
[ "$checksum_count" -eq 1 ] || fail "checksum manifest must contain exactly one entry for ${archive_name}"

expected_checksum=$(awk -v name="$archive_name" '$2 == name { print $1 }' "$checksums_path")
case "$expected_checksum" in
    "" | *[!0-9a-f]*)
        fail "checksum manifest contains an invalid SHA-256 digest"
        ;;
esac
[ "${#expected_checksum}" -eq 64 ] || fail "checksum manifest contains an invalid SHA-256 digest"

case "$hash_command" in
    shasum)
        actual_checksum=$(shasum -a 256 "$archive_path" | awk '{ print $1 }')
        ;;
    sha256sum)
        actual_checksum=$(sha256sum "$archive_path" | awk '{ print $1 }')
        ;;
    *)
        fail "internal checksum command selection error"
        ;;
esac

[ "$actual_checksum" = "$expected_checksum" ] || fail "SHA-256 verification failed for ${archive_name}"

mkdir -p "$extract_directory"
tar -xzf "$archive_path" -C "$extract_directory"

binary_path="${extract_directory}/taiko-${target}/taiko"
[ -f "$binary_path" ] || fail "release archive does not contain the taiko binary"
chmod 0755 "$binary_path"
version=$("$binary_path" --version) || fail "downloaded taiko binary could not run on this system"

mkdir -p "$install_directory"
staged_destination=$(mktemp "${install_directory}/.taiko.XXXXXX")
install -m 0755 "$binary_path" "$staged_destination"
destination="${install_directory}/taiko"
mv -f "$staged_destination" "$destination"
staged_destination=""

say "installed ${version} at ${destination}"
case ":${PATH:-}:" in
    *":${install_directory}:"*)
        ;;
    *)
        say "add ${install_directory} to PATH before running taiko"
        ;;
esac
