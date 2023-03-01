FROM rust:1-slim-buster AS builder

WORKDIR /usr/src/poller

RUN apt-get update && apt-get -y install pkg-config libssl-dev
RUN rustup default nightly

COPY ./Cargo.toml ./Cargo.toml
COPY ./src ./src

RUN cargo build --release

CMD cp target/release/jaspy-poller /output/jaspy-poller
