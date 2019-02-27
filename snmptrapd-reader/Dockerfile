FROM alpine:latest AS builder

WORKDIR /usr/src/snmptrapd-reader
COPY . .
RUN apk add --no-cache libgcc openssl-dev pkgconfig rust cargo && \
    cargo build --release

FROM alpine:latest
COPY --from=builder /usr/src/snmptrapd-reader/target/release/jaspy-snmptrapd-reader /usr/bin/jaspy-snmptrapd-reader
RUN apk --no-cache add net-snmp && \
		echo "traphandle IF-MIB::linkUp /usr/bin/jaspy-snmptrapd-reader ${JASPY_URL}" >> /etc/snmp/snmptrapd.conf && \
		echo "traphandle IF-MIB::linkDown /usr/bin/jaspy-snmptrapd-reader ${JASPY_URL}" >> /etc/snmp/snmptrapd.conf

ENTRYPOINT ["snmptrapd", "-L", "o", "-f"]
