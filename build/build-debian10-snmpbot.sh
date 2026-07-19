#!/bin/bash -e
#
# Builds a refreshed snmpbot .deb for the legacy Debian 10 (buster) amd64 VM,
# carrying the current repo MIB set so the merged nexus's STP/VLAN/LAG/wireless
# SNMP queries resolve (the installed snmpbot 2.2.0-39 ships an older, smaller
# MIB set lacking jaspyStpBridgeTable / jaspyVlanTrunkPortTable etc.).
#
# snmpbot's binary is UPSTREAM github.com/qmsk/snmpbot (not jaspy code) and can
# no longer be built from source on EOL buster (buster Go 1.11 vs upstream's
# io/fs / Go >= 1.16 requirement). Since only the MIBs need to change, this
# REPACKAGES the deployed snmpbot: the binary and the systemd unit are taken
# from the running host (the unit preserves operational flags such as
# -snmp-timeout that the repo unit lacks), and the MIBs come from the repo
# (snmpbot/mibs, the authoritative source). Same package name/layout/maintainer
# as the installed snmpbot, so dpkg sees a normal version-bump upgrade.
#
# Binary + unit are fetched over ssh from the deployed host by default. For an
# offline build, point SNMPBOT_BINARY and SNMPBOT_UNIT at local copies instead.
#
# Usage:  cd build && ./build-debian10-snmpbot.sh
#         SNMPBOT_SSH=user@host ./build-debian10-snmpbot.sh
#         SNMPBOT_BINARY=/path/snmpbot SNMPBOT_UNIT=/path/snmpbot.service ./build-debian10-snmpbot.sh
# Output: build/output/debian10/snmpbot_<version>_amd64.deb

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_dir="$(cd "${script_dir}/.." && pwd)"
stage="${script_dir}/output/debian10-snmpbot"
out="${script_dir}/output/debian10"
version="$(cd "${repo_dir}" && git describe --tags | grep -oE '[0-9].+')"
SNMPBOT_SSH="${SNMPBOT_SSH:-garo@mobydick.netcrew.fi}"

echo "==="
echo "=== snmpbot Debian 10 (buster) amd64 repackage — version ${version}"
echo "==="

rm -rf "${stage}"
mkdir -p "${stage}/root/usr/bin" "${stage}/root/etc/systemd/system" "${stage}/root/var/lib/snmpbot"

# --- Binary: reuse the deployed upstream snmpbot -----------------------------
if [ -n "${SNMPBOT_BINARY:-}" ]; then
  echo "=== using local snmpbot binary: ${SNMPBOT_BINARY}"
  cp "${SNMPBOT_BINARY}" "${stage}/root/usr/bin/snmpbot"
else
  echo "=== fetching snmpbot binary from ${SNMPBOT_SSH}:/usr/bin/snmpbot"
  scp -q "${SNMPBOT_SSH}:/usr/bin/snmpbot" "${stage}/root/usr/bin/snmpbot"
fi
# Ensure it is world-executable (avoids the 0700 trap that bit the nexus deb).
chmod 755 "${stage}/root/usr/bin/snmpbot"

# --- Service unit: reuse the deployed one (keeps -snmp-timeout etc.) ---------
if [ -n "${SNMPBOT_UNIT:-}" ]; then
  echo "=== using local snmpbot unit: ${SNMPBOT_UNIT}"
  cp "${SNMPBOT_UNIT}" "${stage}/root/etc/systemd/system/snmpbot.service"
else
  echo "=== fetching snmpbot.service from ${SNMPBOT_SSH}"
  scp -q "${SNMPBOT_SSH}:/etc/systemd/system/snmpbot.service" "${stage}/root/etc/systemd/system/snmpbot.service"
fi

# --- MIBs: the current repo set (authoritative) -----------------------------
echo "=== staging repo MIBs (snmpbot/mibs)"
cp -a "${repo_dir}/snmpbot/mibs" "${stage}/root/var/lib/snmpbot/mibs"
echo "staged $(ls "${stage}/root/var/lib/snmpbot/mibs" | wc -l | tr -d ' ') MIB files"

# --- Package (fpm; native build, no compilation) ----------------------------
mkdir -p "${out}"
echo "=== packaging snmpbot .deb"
docker build \
  -f "${script_dir}/debian/Dockerfile.snmpbot" \
  --build-arg version="${version}" \
  -t "jaspy/snmpbot_deb_debian10:${version}" \
  "${script_dir}"

docker run --rm -v "${out}:/output" "jaspy/snmpbot_deb_debian10:${version}"

echo "==="
echo "=== done. Artifacts in ${out}:"
ls -la "${out}"/snmpbot_*.deb
