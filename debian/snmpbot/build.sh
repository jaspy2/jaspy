#!/bin/bash -e
tmpdir=$(mktemp -d)
echo ${tmpdir}
export GOPATH="${tmpdir}"
go get github.com/qmsk/snmpbot/cmd/snmpbot
version=$(git --git-dir="${tmpdir}/src/github.com/qmsk/snmpbot/.git" describe --tags | grep -oE '[0-9].+')
fpm --force -t deb -s dir -n "snmpbot" -v "${version}" -m "Antti Tönkyrä <daedalus@pingtimeout.net>" --after-install ./postinst.sh systemd/system/=/etc/systemd/system/ ${tmpdir}/bin/=/usr/bin/
rm -rf ${tmpdir}
