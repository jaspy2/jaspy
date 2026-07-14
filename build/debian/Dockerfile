ARG DEBIAN_VERSION=bullseye-slim

FROM debian:${DEBIAN_VERSION}

WORKDIR /usr/src/jaspy/root

RUN apt-get update && apt-get -y install ruby ruby-dev libffi-dev build-essential
RUN gem install fpm

ARG version=unknown
ENV VERSION=$version

COPY output/cli ./usr/local/bin
COPY output/weathermap ./var/lib/jaspy/weathermap

COPY output/nexus/migrations ./var/lib/jaspy/nexus/migrations
COPY output/nexus/jaspy-nexus ./usr/lib/jaspy/jaspy-nexus
COPY output/nexus/diesel ./usr/lib/jaspy/diesel

RUN mkdir ./usr/lib/jaspy/switchmaster
COPY output/switchmaster/* ./usr/lib/jaspy/switchmaster/

COPY debian/systemd.jaspy/*.service ./etc/systemd/system/

WORKDIR /usr/src/jaspy
COPY debian/jaspy.*.sh ./
RUN fpm -t deb -s dir -n "jaspy" -v "${version}" -m "Antti Tönkyrä <daedalus@pingtimeout.net>" -d liboping0 -d libpq5 --after-remove ./jaspy.postremove.sh --after-install ./jaspy.postinst.sh --after-upgrade ./jaspy.postupgrade.sh /usr/src/jaspy/root/=/

WORKDIR /usr/src/snmpbot/root
COPY output/snmpbot/snmpbot ./usr/bin/snmpbot
COPY output/snmpbot/mibs ./var/lib/snmpbot/mibs
COPY debian/systemd.snmpbot/*.service ./etc/systemd/system/

WORKDIR /usr/src/snmpbot
COPY debian/snmpbot.*.sh ./
RUN fpm -t deb -s dir -n "snmpbot" -v "${version}" -m "Antti Tönkyrä <daedalus@pingtimeout.net>" --after-install ./snmpbot.postinst.sh /usr/src/snmpbot/root/=/

CMD cp -a /usr/src/jaspy/*.deb /usr/src/snmpbot/*.deb /output/
