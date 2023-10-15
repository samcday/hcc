FROM rust:1-alpine3.18 AS builder
RUN apk add --no-cache musl-dev openssl-dev

WORKDIR /usr/src/hcc
COPY . .

# https://github.com/rust-lang/rust/issues/115430
ENV RUSTFLAGS="-Ctarget-feature=-crt-static"

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/src/hcc/target \
    cargo install --path .

FROM alpine:3.18
RUN apk add --no-cache libgcc
COPY --from=builder /usr/local/cargo/bin/hcc /usr/local/bin/hcc
CMD ["hcc"]
