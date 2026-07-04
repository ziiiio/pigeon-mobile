# Dev container for pigeon-mobile.
#
# Carries the full toolchain to build the Rust core, cross-compile it for
# Android (cargo-ndk), generate UniFFI bindings, and assemble the Android app:
#   - Rust + rustfmt/clippy + Android targets + cargo-ndk
#   - JDK 25 (Temurin) + Android SDK (platform 36, build-tools 36) + NDK r28c
#   - Gradle (used to materialise the project's wrapper)
#
# Toolchain matches AGP 9.2.1 / Gradle 9.6.1 / Kotlin 2.2.10 (see android/ and
# CLAUDE.md): AGP 9 needs a modern JDK with a compiler and SDK 36, so `assemble
# Debug` runs in here — no longer a host-only step.
#
# The host's `projects/` directory is bind-mounted at /workspace so both this
# repo and the sibling `pigeon` repo (the path-dependency source) are visible.
# Running an Android *emulator* in here is out of scope (no KVM/display); the
# image builds and assembles, the emulator runs on the host / Android Studio.
FROM rust:1-bookworm

# --- Base build tooling -------------------------------------------------------
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential \
        pkg-config \
        libssl-dev \
        git \
        python3 \
        unzip \
        wget \
        ca-certificates \
        gnupg \
        apt-transport-https \
    && rm -rf /var/lib/apt/lists/*

# --- JDK 25 (Temurin) ---------------------------------------------------------
# AGP 9 / Gradle 9 need a modern JDK with a compiler (a JRE isn't enough, and
# Gradle 9 can't run on JDK 25 via an 8.x wrapper — see CLAUDE.md). Installed
# from Adoptium's apt repo so it tracks security updates.
RUN install -d /etc/apt/keyrings \
    && wget -qO - https://packages.adoptium.net/artifactory/api/gpg/key/public \
       | gpg --dearmor -o /etc/apt/keyrings/adoptium.gpg \
    && echo "deb [signed-by=/etc/apt/keyrings/adoptium.gpg] https://packages.adoptium.net/artifactory/deb bookworm main" \
       > /etc/apt/sources.list.d/adoptium.list \
    && apt-get update \
    && apt-get install -y --no-install-recommends temurin-25-jdk \
    && rm -rf /var/lib/apt/lists/*
ENV JAVA_HOME=/usr/lib/jvm/temurin-25-jdk-amd64
ENV PATH="${JAVA_HOME}/bin:${PATH}"

# Lint/format components (CI gates on `cargo fmt --check` + `cargo clippy`).
RUN rustup component add rustfmt clippy

# --- Android SDK + NDK --------------------------------------------------------
ENV ANDROID_SDK_ROOT=/opt/android-sdk \
    ANDROID_HOME=/opt/android-sdk
ENV PATH="${PATH}:${ANDROID_SDK_ROOT}/cmdline-tools/latest/bin:${ANDROID_SDK_ROOT}/platform-tools"

ARG ANDROID_CMDLINE_TOOLS=commandlinetools-linux-11076708_latest.zip
ARG ANDROID_NDK_VERSION=28.2.13676358
RUN mkdir -p ${ANDROID_SDK_ROOT}/cmdline-tools \
    && wget -q https://dl.google.com/android/repository/${ANDROID_CMDLINE_TOOLS} -O /tmp/cmdline.zip \
    && unzip -q /tmp/cmdline.zip -d ${ANDROID_SDK_ROOT}/cmdline-tools \
    && mv ${ANDROID_SDK_ROOT}/cmdline-tools/cmdline-tools ${ANDROID_SDK_ROOT}/cmdline-tools/latest \
    && rm /tmp/cmdline.zip \
    && yes | sdkmanager --licenses > /dev/null \
    && sdkmanager --install \
        "platform-tools" \
        "platforms;android-36" \
        "build-tools;36.0.0" \
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
# Matches the project wrapper (9.6.1); Gradle 8.x can't run on JDK 25.
ARG GRADLE_VERSION=9.6.1
RUN wget -q https://services.gradle.org/distributions/gradle-${GRADLE_VERSION}-bin.zip -O /tmp/gradle.zip \
    && unzip -q /tmp/gradle.zip -d /opt \
    && rm /tmp/gradle.zip
ENV PATH="${PATH}:/opt/gradle-${GRADLE_VERSION}/bin"

WORKDIR /workspace/pigeon-mobile