#!/bin/sh
set -eu

script_directory=$(CDPATH='' cd "$(dirname "$0")" && pwd)
repository_root=$(CDPATH='' cd "${script_directory}/.." && pwd)
temporary_directory=$(mktemp -d "${TMPDIR:-/tmp}/taiko-installer-test.XXXXXX")

cleanup() {
    rm -rf "$temporary_directory"
}

trap cleanup 0
trap 'exit 129' 1
trap 'exit 130' 2
trap 'exit 143' 15

case "$(uname -s):$(uname -m)" in
    Darwin:arm64 | Darwin:aarch64)
        target="aarch64-apple-darwin"
        ;;
    Darwin:x86_64)
        target="x86_64-apple-darwin"
        ;;
    Linux:x86_64 | Linux:amd64)
        target="x86_64-unknown-linux-gnu"
        ;;
    *)
        printf '%s\n' "installer test does not support this host" >&2
        exit 1
        ;;
esac

fixture_binary="${temporary_directory}/taiko"
fixture_release="${temporary_directory}/release"
mock_directory="${temporary_directory}/mock-bin"
install_directory="${temporary_directory}/install/bin"
curl_log="${temporary_directory}/curl.log"

mkdir -p "$fixture_release" "$mock_directory"

# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'if [ "${1:-}" = "--version" ]; then' \
    '    printf "%s\n" "taiko 0.1.0-test"' \
    '    exit 0' \
    'fi' \
    'exit 2' > "$fixture_binary"
chmod 0755 "$fixture_binary"

sh "${repository_root}/scripts/package-release.sh" \
    "$target" \
    "$fixture_binary" \
    "$fixture_release" >/dev/null

archive_name="taiko-${target}.tar.gz"
if [ "$(uname -s)" = "Darwin" ]; then
    (
        cd "$fixture_release"
        shasum -a 256 "$archive_name" > SHA256SUMS
    )
else
    (
        cd "$fixture_release"
        sha256sum "$archive_name" > SHA256SUMS
    )
fi

# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'output_path=""' \
    'request_url=""' \
    'while [ "$#" -gt 0 ]; do' \
    '    case "$1" in' \
    '        --output)' \
    '            output_path=$2' \
    '            shift 2' \
    '            ;;' \
    '        --proto)' \
    '            shift 2' \
    '            ;;' \
    '        --tlsv1.2 | --fail | --silent | --show-error | --location)' \
    '            shift' \
    '            ;;' \
    '        https://*)' \
    '            request_url=$1' \
    '            shift' \
    '            ;;' \
    '        *)' \
    '            printf "%s\n" "unexpected curl argument: $1" >&2' \
    '            exit 2' \
    '            ;;' \
    '    esac' \
    'done' \
    '[ -n "$output_path" ] && [ -n "$request_url" ]' \
    'asset_name=${request_url##*/}' \
    'printf "%s\n" "$request_url" >> "$MOCK_CURL_LOG"' \
    'cp "${MOCK_RELEASE_DIRECTORY}/${asset_name}" "$output_path"' > "${mock_directory}/curl"
chmod 0755 "${mock_directory}/curl"

MOCK_RELEASE_DIRECTORY="$fixture_release" \
MOCK_CURL_LOG="$curl_log" \
TAIKO_INSTALL_DIR="$install_directory" \
PATH="${mock_directory}:${PATH}" \
    sh "${repository_root}/install.sh" >/dev/null

installed_binary="${install_directory}/taiko"
[ -x "$installed_binary" ]
[ "$("$installed_binary" --version)" = "taiko 0.1.0-test" ]
grep --fixed-strings --line-regexp --quiet \
    "https://github.com/JacobLinCool/rhythm-rs/releases/download/latest/${archive_name}" \
    "$curl_log"
grep --fixed-strings --line-regexp --quiet \
    "https://github.com/JacobLinCool/rhythm-rs/releases/download/latest/SHA256SUMS" \
    "$curl_log"

bad_release="${temporary_directory}/bad-release"
bad_install="${temporary_directory}/bad-install/bin"
mkdir -p "$bad_release"
cp "${fixture_release}/${archive_name}" "$bad_release"
printf '%064d  %s\n' 0 "$archive_name" > "${bad_release}/SHA256SUMS"

if MOCK_RELEASE_DIRECTORY="$bad_release" \
    MOCK_CURL_LOG="$curl_log" \
    TAIKO_INSTALL_DIR="$bad_install" \
    PATH="${mock_directory}:${PATH}" \
    sh "${repository_root}/install.sh" >/dev/null 2>&1; then
    printf '%s\n' "installer accepted an invalid checksum" >&2
    exit 1
fi
[ ! -e "${bad_install}/taiko" ]

unsupported_mock="${temporary_directory}/unsupported-bin"
mkdir -p "$unsupported_mock"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'case "${1:-}" in' \
    '    -s) printf "%s\n" "Plan9" ;;' \
    '    -m) printf "%s\n" "mips" ;;' \
    '    *) exit 2 ;;' \
    'esac' > "${unsupported_mock}/uname"
chmod 0755 "${unsupported_mock}/uname"

if TAIKO_INSTALL_DIR="${temporary_directory}/unsupported-install" \
    PATH="${unsupported_mock}:${PATH}" \
    sh "${repository_root}/install.sh" >/dev/null 2>&1; then
    printf '%s\n' "installer accepted an unsupported platform" >&2
    exit 1
fi

printf '%s\n' "installer tests passed for ${target}"
