FROM rust:1.32.0-slim-stretch AS builder
WORKDIR /usr/src/nexus
COPY . .
RUN apt-get update && \
    apt-get install -y libzmq3-dev default-libmysqlclient-dev libpq-dev libsqlite3-dev pkg-config && \
    # Temporarily bump to nightly because of Pear
    rustup update nightly && \
    rustup default nightly && \
  	cargo install diesel_cli && \
		cargo build --release

FROM busybox:glibc
COPY --from=builder /usr/src/nexus/target/release/jaspy-nexus /usr/bin/jaspy-nexus
COPY ./entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh
ENTRYPOINT ["/entrypoint.sh"]
