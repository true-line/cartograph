# Linux CI image for the Rust lanes (ci.yml). The Tauri/webkit system
# libraries and the pinned Rust toolchain are baked in, so a CI run downloads
# nothing from the Ubuntu archive or static.rust-lang.org. ci.yml tags the
# image with a hash of this file and rust-toolchain.toml
# (scripts/ci-image-tag.mjs) and builds it only when that tag is missing.
FROM ubuntu:26.04

ENV DEBIAN_FRONTEND=noninteractive \
    RUSTUP_HOME=/opt/rustup

# One apt source, read in one update, so the index and the archives it names
# always come from the same mirror.
RUN apt-get -o Acquire::Retries=5 update \
 && apt-get -o Acquire::Retries=5 install -y \
      ca-certificates git curl wget file xz-utils zstd \
      build-essential pkg-config \
      libwebkit2gtk-4.1-dev libxdo-dev libssl-dev \
      libayatana-appindicator3-dev librsvg2-dev \
      rustup \
 && rm -rf /var/lib/apt/lists/*

COPY rust-toolchain.toml /opt/toolchain/rust-toolchain.toml
RUN cd /opt/toolchain \
 && channel=$(sed -n 's/^channel = "\(.*\)"$/\1/p' rust-toolchain.toml) \
 && rustup toolchain install "$channel" --profile minimal --component rustfmt,clippy \
 && rustup default "$channel" \
 && cargo --version && cargo clippy --version && cargo fmt --version

# Jobs run as the hosted runner's uid (ci.yml `--user 1001`), not root: root
# would pass the tests that expect a permission-denied error, and files the
# job writes to the mounted workspace stay owned by the runner.
RUN useradd --uid 1001 --create-home ci \
 && chown -R 1001:1001 "$RUSTUP_HOME"
USER 1001
