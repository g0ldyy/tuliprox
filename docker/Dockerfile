# Build rust stage
FROM rust:bookworm AS rust-build

# Get target architecture
ARG RUST_TARGET

RUN apt-get update && apt-get install -y --no-install-recommends pkg-config musl-tools libssl-dev

# Update Rust toolchain and add necessary target
RUN rustup update && rustup target add $RUST_TARGET

# Set Rust compiler flags for better optimization and reproducibility
ENV RUSTFLAGS='--remap-path-prefix $HOME=~ -C target-feature=+crt-static'

# Copy only Cargo.toml and Cargo.lock for dependency caching
WORKDIR /src
COPY Cargo.toml Cargo.lock ./

# Create a dummy src/main.rs to build dependencies only
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN cargo build --target $RUST_TARGET --release || true

# Now copy the actual source code and build the project
COPY . .
RUN cargo build --target $RUST_TARGET --release

# Build node stage
FROM node:lts AS node-build

ENV NODE_OPTIONS=--openssl-legacy-provider

WORKDIR /app

# Copy package.json files first for better caching
COPY ./frontend/package.json ./

# Install dependencies
RUN yarn install --prefer-offline --non-interactive --production=false

# Copy the rest of the frontend code
COPY ./frontend /app

# Build the frontend
RUN yarn build && rm -rf node_modules

# Build resource stage (using ffmpeg)
FROM linuxserver/ffmpeg:latest AS resource-build

WORKDIR /src
COPY resources ./resources

# Combine ffmpeg commands into a single layer to reduce image size
RUN ffmpeg -loop 1 -i ./resources/channel_unavailable.jpg -t 10 -r 1 -an \
    -vf "scale=1920:1080" \
    -c:v libx264 -preset veryfast -crf 23 -pix_fmt yuv420p \
    ./resources/channel_unavailable.ts && \
  ffmpeg -loop 1 -i ./resources/user_connections_exhausted.jpg -t 10 -r 1 -an \
    -vf "scale=1920:1080" \
    -c:v libx264 -preset veryfast -crf 23 -pix_fmt yuv420p \
    ./resources/user_connections_exhausted.ts && \
  ffmpeg -loop 1 -i ./resources/provider_connections_exhausted.jpg -t 10 -r 1 -an \
    -vf "scale=1920:1080" \
    -c:v libx264 -preset veryfast -crf 23 -pix_fmt yuv420p \
    ./resources/provider_connections_exhausted.ts && \
   ffmpeg -loop 1 -i ./resources/user_account_expired.jpg -t 10 -r 1 -an \
     -vf "scale=1920:1080" \
     -c:v libx264 -preset veryfast -crf 23 -pix_fmt yuv420p \
     ./resources/user_account_expired.ts

# Stage to prepare /etc/localtime for scratch
FROM alpine:latest AS tz-prep

ARG TZ=UTC
ENV TZ=${TZ}

RUN apk add --no-cache tzdata \
  && mkdir -p /output/etc \
  && mkdir -p /output/usr/share \
  && cp -r /usr/share/zoneinfo /output/usr/share/zoneinfo \
  && ln -sf /usr/share/zoneinfo/${TZ} /output/etc/localtime


# Final container (Scratch)
FROM scratch AS scratch-final

ARG RUST_TARGET

# Copy timezone data and localtime from tz-prep
COPY --from=tz-prep /output/usr/share/zoneinfo /usr/share/zoneinfo
COPY --from=tz-prep /output/etc/localtime /etc/localtime

# Certificates
COPY --from=rust-build /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/

# RUN ln -sf /usr/share/zoneinfo/${TZ} /etc/localtime

# App
WORKDIR /app
COPY --from=rust-build /src/target/${RUST_TARGET}/release/tuliprox ./tuliprox
COPY --from=node-build /app/build ./web
COPY --from=resource-build /src/resources ./resources

ENTRYPOINT ["/app/tuliprox"]
CMD ["-s", "-p", "/app/config"]

# Final container (Alpine)
FROM alpine:latest AS alpine-final

ARG RUST_TARGET
ARG TZ=UTC
ENV TZ=${TZ}

RUN apk add --no-cache bash curl ca-certificates tini

COPY --from=rust-build /usr/share/zoneinfo /usr/share/zoneinfo
COPY --from=rust-build /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/

# Default timezone fallback
RUN ln -sf /usr/share/zoneinfo/${TZ} /etc/localtime

WORKDIR /app
COPY --from=rust-build /src/target/${RUST_TARGET}/release/tuliprox ./tuliprox
COPY --from=node-build /app/build ./web
COPY --from=resource-build /src/resources ./resources
ENTRYPOINT ["/sbin/tini", "--", "/app/tuliprox"]
CMD ["-s", "-p", "/app/config"]
