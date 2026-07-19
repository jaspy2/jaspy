#!/bin/bash -e
#
# Builds the standalone jaspy-switchmaster .deb for the legacy Debian 10 (buster)
# amd64 VM. switchmaster was split out of the monolithic jaspy package during the
# nexus merge; everything else now lives in jaspy-nexus (build-debian10.sh).
#
# This REPACKAGES switchmaster from the VM backup tarball rather than building
# from source: switchmaster/Dockerfile's .NET Core 2.2 / Debian stretch toolchain
# is EOL and no longer apt-installable. switchmaster is a self-contained .NET
# publish (bundled runtime), so the backed-up binaries are a valid drop-in.
#
# Prerequisite: backup/jaspy-backup.tar.gz — a tarball of the VM's installed
# files containing usr/lib/jaspy/switchmaster/. (Made from the running Debian 10
# host; the original .deb was lost.)
#
# Usage:  cd build && ./build-debian10-switchmaster.sh
# Output: build/output/debian10/jaspy-switchmaster_<version>_amd64.deb

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_dir="$(cd "${script_dir}/.." && pwd)"
backup="${repo_dir}/backup/jaspy-backup.tar.gz"
stage="${script_dir}/output/debian10-switchmaster"
out="${script_dir}/output/debian10"
version="$(cd "${repo_dir}" && git describe --tags | grep -oE '[0-9].+')"

echo "==="
echo "=== jaspy-switchmaster Debian 10 (buster) amd64 repackage — version ${version}"
echo "==="

if [ ! -f "${backup}" ]; then
  echo "ERROR: backup tarball not found at ${backup}" >&2
  echo "It must contain usr/lib/jaspy/switchmaster/ from the VM." >&2
  exit 1
fi

# --- Stage the switchmaster payload from the backup -------------------------
rm -rf "${stage}"
mkdir -p "${stage}/root"
echo "=== extracting usr/lib/jaspy/switchmaster from the backup"
tar xzf "${backup}" -C "${stage}/root" usr/lib/jaspy/switchmaster
if [ ! -x "${stage}/root/usr/lib/jaspy/switchmaster/Jaspy.Switchmaster" ]; then
  echo "ERROR: Jaspy.Switchmaster executable not found in the backup payload" >&2
  exit 1
fi
echo "staged $(find "${stage}/root" -type f | wc -l | tr -d ' ') files"

# --- Package the .deb (fpm; native build, no compilation) -------------------
mkdir -p "${out}"
echo "=== packaging jaspy-switchmaster .deb"
docker build \
  -f "${script_dir}/debian/Dockerfile.switchmaster" \
  --build-arg version="${version}" \
  -t "jaspy/switchmaster_deb_debian10:${version}" \
  "${script_dir}"

docker run --rm -v "${out}:/output" "jaspy/switchmaster_deb_debian10:${version}"

echo "==="
echo "=== done. Artifacts in ${out}:"
ls -la "${out}"/jaspy-switchmaster_*.deb
