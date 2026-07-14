FROM rust:1-slim-bookworm AS builder

WORKDIR /usr/src/nexus

RUN apt-get update
RUN apt-get -y install man libpq-dev pkg-config postgresql libssl-dev liboping-dev

RUN for man in $(seq 0 9); do mkdir -p /usr/share/man/man${man}; done

# nexus now builds on stable Rust (Rocket 0.5 / diesel 2); rust-toolchain.toml
# pins the channel. diesel_cli v2 is needed to generate src/schema.rs.
RUN cargo install diesel_cli --no-default-features --features "postgres"

COPY ./Cargo.toml ./Cargo.toml
COPY ./Cargo.lock ./Cargo.lock
COPY ./rust-toolchain.toml ./rust-toolchain.toml
COPY ./src ./src
COPY ./diesel.toml ./diesel.toml
COPY ./migrations ./migrations

# bookworm ships postgresql 15 (buster shipped 11)
RUN su -c 'pg_ctlcluster 15 main start && createuser root && createdb root -O root' postgres && diesel --database-url postgresql:///root migration run && diesel --database-url postgresql:///root print-schema

RUN cat /usr/src/nexus/src/schema.rs
RUN cargo build --release

CMD cp -a target/release/jaspy-nexus /output/jaspy-nexus && cp -a /usr/local/cargo/bin/diesel /output/diesel && cp -a migrations /output/migrations
