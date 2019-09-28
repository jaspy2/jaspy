FROM debian:buster-slim AS builder
WORKDIR /usr/src/entitypoller
RUN apt-get update && apt-get -y install golang-go git
ENV GOPATH="/usr/src/entitypoller/build"
ENV CGO_ENABLED=0
ENV GOOS=linux
COPY src /usr/src/entitypoller/build/src/github.com/jaspy2/jaspy/entitypoller
COPY Gopkg.lock /usr/src/entitypoller/build/src/github.com/jaspy2/jaspy/entitypoller/Gopkg.lock
COPY Gopkg.toml /usr/src/entitypoller/build/src/github.com/jaspy2/jaspy/entitypoller/Gopkg.toml
WORKDIR /usr/src/entitypoller/build/src/github.com/jaspy2/jaspy/entitypoller
RUN go get -u github.com/golang/dep/cmd/dep
RUN /usr/src/entitypoller/build/bin/dep ensure -vendor-only
RUN go build -a -ldflags '-extldflags "-static"' -o /usr/src/entitypoller/build/entitypoller-linux github.com/jaspy2/jaspy/entitypoller
CMD cp -a /usr/src/entitypoller/build/entitypoller-linux /output/entitypoller
