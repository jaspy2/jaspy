FROM rust:1-slim-stretch AS nexus_builder
WORKDIR /usr/src/nexus
RUN apt-get update && apt-get -y install libpq-dev pkg-config
RUN rustup default nightly
COPY . .
RUN cargo build --release

