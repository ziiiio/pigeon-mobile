# Dev container for pigeon-mobile.
#
# Scope (Phase M0.1 / M0.2): build + test the Rust core (`pigeon-mobile-core`)
# against the reused homeserver crates, and run UniFFI binding generation.
# The Android NDK layer (cargo-ndk, the per-ABI .so cross-compile — M0.3) and a
# JDK/Gradle layer (the app build — M0.4+) are added here when those stages land.
#
# The host's `projects/` directory is bind-mounted at /workspace so both this
# repo and the sibling `pigeon` repo (the path-dependency source) are visible.
FROM rust:1-bookworm

RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential \
        pkg-config \
        libssl-dev \
        git \
        python3 \
        unzip \
    && rm -rf /var/lib/apt/lists/*

# Lint/format components (CI gates on `cargo fmt --check` + `cargo clippy`).
RUN rustup component add rustfmt clippy

# uniffi-bindgen is built from the core crate's own bin (library mode), so no
# global install is needed here — keeping the bindgen version pinned to the lib.

WORKDIR /workspace/pigeon-mobile