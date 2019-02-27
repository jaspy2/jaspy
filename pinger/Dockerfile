FROM alpine:latest AS builder

WORKDIR /usr/src/jaspy-pinger
COPY . .
RUN apk add --no-cache alpine-sdk autoconf automake cargo libtool m4 openssl-dev rust && \
    cargo build --release

FROM alpine:latest
RUN apk add --no-cache libgcc
COPY --from=builder /usr/src/jaspy-pinger/target/release/jaspy-pinger /usr/bin/jaspy-pinger
ENTRYPOINT ["jaspy-pinger"]
