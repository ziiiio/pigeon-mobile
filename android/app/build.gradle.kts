import org.gradle.api.tasks.Exec

plugins {
    id("com.android.application")
    // Kotlin support is built into AGP 9 (no `kotlin.android` plugin). Only the
    // Compose compiler plugin is applied on top.
    id("org.jetbrains.kotlin.plugin.compose")
}

// --- Rust core integration (M0.3 cross-compile + M0.5 build glue) ------------
// `./gradlew assemble<BuildType>` cross-compiles the Rust core with cargo-ndk for
// that build type, then regenerates the UniFFI Kotlin bindings, so the app build
// is one command. Debug and release each get their own cargo profile and output
// directory, so a release APK packages an optimized core (Gotcha #10), not the
// debug one, and a debug build stays exactly as fast as before.
val coreDir = rootProject.file("../core")
val uniffiDir = layout.buildDirectory.dir("generated/uniffi")

/** Per build type: the cargo profile flags, its target dir name, and the env. */
data class CoreProfile(
    val buildType: String,      // "debug" / "release" (the AGP build type)
    val cargoArgs: List<String>, // extra `cargo build` args
    val targetSubdir: String,    // cargo's output dir under target/<triple>/
    val env: Map<String, String>,
)

val coreProfiles = listOf(
    // Strip DWARF debug info from the debug .so (symbol tables stay, so UniFFI's
    // library-mode bindgen and panic backtraces still work). Unstripped, the core
    // is ~145 MB per ABI: a ~450 MB debug APK that took minutes to install and
    // no longer fit on a default 6 GB emulator (INSTALL_FAILED_INSUFFICIENT_STORAGE).
    // With the pinned NDK r28c, AGP's stripDebugDebugSymbols then strips the
    // packaged copy further (16.1 MiB arm64 in the APK); this override still keeps
    // build/rustJniLibs small and doesn't depend on AGP finding its NDK (an older
    // image with NDK 26.3 skipped that step silently).
    CoreProfile("debug", emptyList(), "debug", mapOf("CARGO_PROFILE_DEV_STRIP" to "debuginfo")),
    // The release core: optimized, DWARF stripped, symbols kept for bindgen.
    CoreProfile("release", listOf("--release"), "release", mapOf("CARGO_PROFILE_RELEASE_STRIP" to "debuginfo")),
)
// Only these cross-builds get the strip overrides; host builds (tests, bindgen)
// are unaffected.

fun String.cap() = replaceFirstChar { it.uppercase() }

coreProfiles.forEach { p ->
    val jniLibsDir = layout.buildDirectory.dir("rustJniLibs/${p.buildType}")
    val cargoNdk = tasks.register<Exec>("cargoNdkBuild${p.buildType.cap()}") {
        description = "Cross-compiles pigeon-mobile-core (${p.buildType}) for the target Android ABIs."
        workingDir = coreDir
        commandLine(
            listOf(
                "cargo", "ndk",
                "-t", "arm64-v8a",
                "-t", "x86_64",
                "-o", jniLibsDir.get().asFile.absolutePath,
                "build",
            ) + p.cargoArgs,
        )
        p.env.forEach { (k, v) -> environment(k, v) }
    }
    // Bindings come from the *same* variant's library, so a release build never
    // compiles the debug core just to generate them. The output is identical for
    // either profile (it's read from the same exported metadata), so both variants
    // write the one shared source dir.
    val bindings = tasks.register<Exec>("generateUniffiBindings${p.buildType.cap()}") {
        description = "Generates the UniFFI Kotlin bindings from the ${p.buildType} cdylib."
        dependsOn(cargoNdk)
        workingDir = coreDir
        commandLine(
            "cargo", "run", "--bin", "uniffi-bindgen", "--",
            "generate",
            "--library", "target/aarch64-linux-android/${p.targetSubdir}/libpigeon_mobile_core.so",
            "--language", "kotlin",
            "--out-dir", uniffiDir.get().asFile.absolutePath,
        )
    }
    // AGP 9 registers pre<Variant>Build lazily, so hook it by name as it appears.
    tasks.configureEach {
        if (name == "pre${p.buildType.cap()}Build") dependsOn(bindings)
    }
}

android {
    namespace = "com.pigeon.mobile"
    compileSdk = 36

    defaultConfig {
        applicationId = "com.pigeon.mobile"
        minSdk = 24
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0"
        ndk {
            abiFilters += listOf("arm64-v8a", "x86_64")
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    buildFeatures {
        compose = true
    }

    // Each build type packages its own cargo-ndk output (see coreProfiles above).
    coreProfiles.forEach { p ->
        sourceSets[p.buildType].jniLibs.srcDir(layout.buildDirectory.dir("rustJniLibs/${p.buildType}"))
    }
}

// AGP 9 built-in Kotlin: `kotlinOptions` is gone; set the JVM target through the
// Kotlin compiler options. Bytecode 17 is emitted by the running JDK, so no
// separate JDK 17 toolchain is provisioned.
kotlin {
    compilerOptions {
        jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17)
    }
}

// The UniFFI bindings are pure Kotlin. Under AGP 9's built-in Kotlin, the legacy
// `android.sourceSets[...].java.srcDir` no longer feeds the Kotlin compiler, so
// register the generated dir on each variant's Kotlin sources via the Variant API.
// The dir is pre-created at configuration time (addStaticSourceDirectory requires
// it to exist); content + ordering come from `pre<Variant>Build -> generateUniffiBindings<Variant>`.
androidComponents {
    val uniffiOut = uniffiDir.get().asFile.apply { mkdirs() }
    onVariants { variant ->
        variant.sources.kotlin?.addStaticSourceDirectory(uniffiOut.absolutePath)
    }
}

dependencies {
    // UniFFI-generated Kotlin loads the native library via JNA.
    implementation("net.java.dev.jna:jna:5.14.0@aar")

    implementation("androidx.core:core-ktx:1.13.1")
    implementation("androidx.activity:activity-compose:1.9.2")
    implementation(platform("androidx.compose:compose-bom:2024.09.00"))
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.material3:material3")

    // M1.4 auth UI: ViewModel + coroutines to bridge the core's async FFI, and
    // EncryptedSharedPreferences (Android Keystore-backed) for the KeyStore impl.
    implementation("androidx.lifecycle:lifecycle-viewmodel-compose:2.8.4")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.8.4")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.8.1")
    // NB (flagged dep): androidx.security-crypto wraps the Android Keystore; it is
    // the sanctioned way to store secret bytes at rest — it does not roll its own
    // crypto. Alpha is the current line for this artifact.
    implementation("androidx.security:security-crypto:1.1.0-alpha06")

    testImplementation("junit:junit:4.13.2")
}