#!/bin/bash -e
tmpdir=$(mktemp -d)
echo ${tmpdir}
which diesel || cargo install diesel_cli --no-default-features --features "postgres"
mkdir -p ${tmpdir}/usr/lib/jaspy
mkdir -p ${tmpdir}/var/lib/jaspy
mkdir -p ${tmpdir}/var/lib/jaspy/nexus
cp -a $(which diesel) ${tmpdir}/usr/lib/jaspy/diesel
for item in nexus poller pinger snmptrapd-reader; do
    pushd ../${item}
    cargo build --release
    cp -a ../target/release/jaspy-${item} ${tmpdir}/usr/lib/jaspy/
    popd
done
cp -a ../discover ${tmpdir}/usr/lib/jaspy/
cp -a ../snmpbot/mibs ${tmpdir}/var/lib/jaspy/
cp -a ../weathermap ${tmpdir}/var/lib/jaspy/
cp -a ../nexus/migrations ${tmpdir}/var/lib/jaspy/nexus/
version=$(git describe --tags)
fpm --force -t deb -s dir -n "jaspy" -v "${version}" -m "Antti Tönkyrä <daedalus@pingtimeout.net>" --after-remove ./postremove.sh --after-install ./postinst.sh systemd/system/=/etc/systemd/system/ ${tmpdir}/=/
rm -rf ${tmpdir}
