FROM rust:1-slim-stretch AS pinger_builder
WORKDIR /usr/src/pinger
RUN apt-get update && apt-get -y install liboping-dev libtool automake autoconf m4 build-essential pkg-config libssl-dev && \
    rustup default nightly
COPY . .
RUN cargo build --release
