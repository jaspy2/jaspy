FROM alpine:latest AS builder

WORKDIR /usr/src/jaspy-poller
COPY . .
RUN apk add --no-cache cargo libgcc openssl-dev rust && \
    cargo build --release

FROM alpine:latest
RUN apk add --no-cache libgcc
COPY --from=builder /usr/src/jaspy-poller/target/release/jaspy-poller /usr/bin/jaspy-poller
ENTRYPOINT ["jaspy-poller"]
