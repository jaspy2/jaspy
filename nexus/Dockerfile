FROM rust:1-slim-buster AS builder
WORKDIR /usr/src/nexus
RUN apt-get update
RUN apt-get -y install man
RUN for man in $(seq 0 9); do mkdir /usr/share/man/man${man}; done
RUN apt-get update && apt-get -y install libpq-dev pkg-config postgresql libssl-dev
RUN rustup default nightly
RUN cargo install diesel_cli --no-default-features --features "postgres"
COPY ./Cargo.toml ./Cargo.toml
COPY ./src ./src
COPY ./diesel.toml ./diesel.toml
COPY ./migrations ./migrations
RUN su -c 'pg_ctlcluster 9.6 main start && createuser root && createdb root -O root' postgres && diesel --database-url postgresql:///root migration run && diesel --database-url postgresql:///root print-schema
RUN cat /usr/src/nexus/src/schema.rs
RUN cargo build --release
CMD cp -a target/release/jaspy-nexus /output/jaspy-nexus && cp -a /usr/local/cargo/bin/diesel /output/diesel && cp -a migrations /output/migrations
