#!/bin/sh
set -eu

if [ "$#" -ne 3 ]; then
    printf '%s\n' "usage: package-release.sh <target> <binary-path> <output-directory>" >&2
    exit 2
fi

target=$1
binary_path=$2
output_directory=$3

case "$target" in
    aarch64-apple-darwin | x86_64-apple-darwin | x86_64-unknown-linux-gnu)
        packaged_binary_name="taiko"
        ;;
    x86_64-pc-windows-msvc)
        packaged_binary_name="taiko.exe"
        ;;
    *)
        printf '%s\n' "unsupported release target: ${target}" >&2
        exit 1
        ;;
esac

[ -f "$binary_path" ] || {
    printf '%s\n' "release binary not found: ${binary_path}" >&2
    exit 1
}

script_directory=$(CDPATH='' cd "$(dirname "$0")" && pwd)
repository_root=$(CDPATH='' cd "${script_directory}/.." && pwd)
temporary_directory=$(mktemp -d "${TMPDIR:-/tmp}/taiko-package.XXXXXX")

cleanup() {
    rm -rf "$temporary_directory"
}

trap cleanup 0
trap 'exit 129' 1
trap 'exit 130' 2
trap 'exit 143' 15

package_name="taiko-${target}"
package_directory="${temporary_directory}/${package_name}"
archive_path="${output_directory}/${package_name}.tar.gz"

mkdir -p "$package_directory" "$output_directory"
cp "$binary_path" "${package_directory}/${packaged_binary_name}"
cp "${repository_root}/LICENSE" "${package_directory}/LICENSE"
cp "${repository_root}/README.md" "${package_directory}/README.md"

COPYFILE_DISABLE=1 tar -C "$temporary_directory" -czf "$archive_path" "$package_name"
[ -s "$archive_path" ] || {
    printf '%s\n' "release archive is empty: ${archive_path}" >&2
    exit 1
}

printf '%s\n' "$archive_path"
