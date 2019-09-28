FROM rust:1-slim-buster AS builder
WORKDIR /usr/src/snmptrapd_reader
RUN apt-get update && apt-get -y install libssl-dev pkg-config
RUN rustup default nightly
COPY . .
RUN cargo build --release
CMD cp target/release/jaspy-snmptrapd-reader /output/jaspy-snmptrapd-reader

