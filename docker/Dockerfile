# Dev container for pigeon-mobile.
#
# Carries the full toolchain to build the Rust core, cross-compile it for
# Android (cargo-ndk), generate UniFFI bindings, and assemble the Android app:
#   - Rust + rustfmt/clippy + Android targets + cargo-ndk
#   - JDK 17 + Android SDK (platform 34, build-tools 34) + NDK
#   - Gradle (used to materialise the project's wrapper)
#
# The host's `projects/` directory is bind-mounted at /workspace so both this
# repo and the sibling `pigeon` repo (the path-dependency source) are visible.
# Running an Android *emulator* in here is out of scope (no KVM/display); the
# image builds and assembles, the emulator runs on the host / Android Studio.
FROM rust:1-bookworm

# --- Base build tooling + JDK -------------------------------------------------
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential \
        pkg-config \
        libssl-dev \
        git \
        python3 \
        unzip \
        wget \
        openjdk-17-jdk-headless \
    && rm -rf /var/lib/apt/lists/*

# Lint/format components (CI gates on `cargo fmt --check` + `cargo clippy`).
RUN rustup component add rustfmt clippy

# --- Android SDK + NDK --------------------------------------------------------
ENV ANDROID_SDK_ROOT=/opt/android-sdk \
    ANDROID_HOME=/opt/android-sdk
ENV PATH="${PATH}:${ANDROID_SDK_ROOT}/cmdline-tools/latest/bin:${ANDROID_SDK_ROOT}/platform-tools"

ARG ANDROID_CMDLINE_TOOLS=commandlinetools-linux-11076708_latest.zip
ARG ANDROID_NDK_VERSION=26.3.11579264
RUN mkdir -p ${ANDROID_SDK_ROOT}/cmdline-tools \
    && wget -q https://dl.google.com/android/repository/${ANDROID_CMDLINE_TOOLS} -O /tmp/cmdline.zip \
    && unzip -q /tmp/cmdline.zip -d ${ANDROID_SDK_ROOT}/cmdline-tools \
    && mv ${ANDROID_SDK_ROOT}/cmdline-tools/cmdline-tools ${ANDROID_SDK_ROOT}/cmdline-tools/latest \
    && rm /tmp/cmdline.zip \
    && yes | sdkmanager --licenses > /dev/null \
    && sdkmanager --install \
        "platform-tools" \
        "platforms;android-34" \
        "build-tools;34.0.0" \
        "ndk;${ANDROID_NDK_VERSION}" > /dev/null
ENV ANDROID_NDK_HOME=${ANDROID_SDK_ROOT}/ndk/${ANDROID_NDK_VERSION} \
    NDK_HOME=${ANDROID_SDK_ROOT}/ndk/${ANDROID_NDK_VERSION}

# --- Rust Android targets + cargo-ndk ----------------------------------------
RUN rustup target add \
        aarch64-linux-android \
        x86_64-linux-android \
        armv7-linux-androideabi
RUN cargo install cargo-ndk --locked

# --- Gradle (to generate the project wrapper) --------------------------------
ARG GRADLE_VERSION=8.9
RUN wget -q https://services.gradle.org/distributions/gradle-${GRADLE_VERSION}-bin.zip -O /tmp/gradle.zip \
    && unzip -q /tmp/gradle.zip -d /opt \
    && rm /tmp/gradle.zip
ENV PATH="${PATH}:/opt/gradle-${GRADLE_VERSION}/bin"

WORKDIR /workspace/pigeon-mobile