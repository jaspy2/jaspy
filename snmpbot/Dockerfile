FROM debian:buster-slim AS builder
WORKDIR /usr/src/snmpbot
RUN apt-get update && apt-get -y install golang-go git
RUN go get github.com/qmsk/snmpbot/cmd/snmpbot
COPY mibs mibs
CMD cp -a /root/go/bin/snmpbot /output/snmpbot && cp -a /usr/src/snmpbot/mibs /output/mibs
