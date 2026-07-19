#!/bin/bash -e
#
# Manual build of the "jaspy-nexus" .deb for the legacy Debian 10 (buster) amd64
# prod VM that cannot be upgraded. Its binaries link buster's glibc 2.28.
#
# The package is named jaspy-nexus with "Replaces: jaspy" (no Conflicts), so it
# installs alongside the old monolithic "jaspy" package via dpkg partial
# replacement: it takes over the nexus files and leaves switchmaster (owned by
# "jaspy") untouched. See build/debian/Dockerfile.debian10 for the rationale.
#
# This is deliberately separate from the CI/CD path: the Kubernetes production
# image is built by nexus/Dockerfile (bookworm) on GitHub and is not touched
# here. Run this by hand when you need a fresh buster .deb.
#
# Requirements: Docker with buildx/QEMU so linux/amd64 can be built on an
# arm64 host (Apple Silicon). The nexus compile runs under emulation and is
# slow (tens of minutes) — that is expected and acceptable for this manual path.
#
# Usage:  cd build && ./build-debian10.sh
# Output: build/output/debian10/jaspy-nexus_<version>_amd64.deb

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_dir="$(cd "${script_dir}/.." && pwd)"
build_target="${script_dir}/output/debian10"
version="$(cd "${repo_dir}" && git describe --tags | grep -oE '[0-9].+')"

platform="linux/amd64"

echo "==="
echo "=== jaspy Debian 10 (buster) amd64 build — version ${version}"
echo "==="

# Clean output for a reproducible run.
rm -rf "${build_target}"
mkdir -p "${build_target}/nexus"

# --- Build nexus for buster/amd64 -------------------------------------------
# --platform is mandatory here: it forces amd64 (this host is arm64) AND the
# buster base pins glibc to 2.28.
echo "=== building nexus (buster/amd64) — this runs under emulation and is slow"
docker build --platform "${platform}" \
  -f "${repo_dir}/nexus/Dockerfile.debian10" \
  --target builder \
  -t "jaspy/nexus_builder_debian10:${version}" \
  "${repo_dir}/nexus"

echo "=== extracting nexus artifacts"
docker run --rm --platform "${platform}" \
  -v "${build_target}/nexus:/output" \
  "jaspy/nexus_builder_debian10:${version}"

# --- Stage the non-compiled artifacts ---------------------------------------
# MIBs (embedded SNMP mode), weathermap (static app) and the CLI (python/shell
# scripts) are architecture-independent, so copy them straight from the tree.
echo "=== staging mibs, weathermap and cli"
cp -a "${repo_dir}/snmpbot/mibs" "${build_target}/nexus/mibs"
cp -a "${repo_dir}/weathermap" "${build_target}/weathermap"
cp -a "${repo_dir}/cli" "${build_target}/cli"

# --- Package the .deb --------------------------------------------------------
# fpm only assembles files, so this stage builds natively (fast); the package
# Architecture is forced to amd64 inside Dockerfile.debian10. The build context
# is build/ so the Dockerfile can COPY output/debian10/... and debian/...
echo "=== packaging jaspy .deb"
docker build \
  -f "${script_dir}/debian/Dockerfile.debian10" \
  --build-arg version="${version}" \
  -t "jaspy/deb_debian10:${version}" \
  "${script_dir}"

docker run --rm \
  -v "${build_target}:/output" \
  "jaspy/deb_debian10:${version}"

echo "==="
echo "=== done. Artifacts in ${build_target}:"
ls -la "${build_target}"/*.deb
echo "==="
echo "Verify glibc requirement on the VM's level before deploy, e.g.:"
echo "  dpkg-deb -x <deb> /tmp/jaspy && objdump -T /tmp/jaspy/usr/lib/jaspy/jaspy-nexus | grep -oE 'GLIBC_[0-9.]+' | sort -uV | tail -1"
echo "Expect <= GLIBC_2.28 for Debian 10 compatibility."
