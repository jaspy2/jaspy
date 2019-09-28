FROM rust:1-slim-buster AS builder
WORKDIR /usr/src/pinger
RUN apt-get update && apt-get -y install liboping-dev libtool automake autoconf m4 build-essential pkg-config libssl-dev
RUN rustup default nightly
COPY . .
RUN cargo build --release
CMD cp target/release/jaspy-pinger /output/jaspy-pinger
