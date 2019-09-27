FROM rust:1-slim-stretch AS poller_builder
WORKDIR /usr/src/poller
RUN apt-get update && apt-get -y install pkg-config libssl-dev
RUN rustup default nightly
COPY . .
RUN cargo build --release
