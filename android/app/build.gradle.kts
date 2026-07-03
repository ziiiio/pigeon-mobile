import org.gradle.api.tasks.Exec

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
}

// --- Rust core integration (M0.3 cross-compile + M0.5 build glue) ------------
// `./gradlew assembleDebug` cross-compiles the Rust core with cargo-ndk, then
// regenerates the UniFFI Kotlin bindings — so the app build is one command.
val coreDir = rootProject.file("../core")
val jniLibsDir = layout.buildDirectory.dir("rustJniLibs")
val uniffiDir = layout.buildDirectory.dir("generated/uniffi")

val cargoNdkBuild by tasks.registering(Exec::class) {
    description = "Cross-compiles pigeon-mobile-core for the target Android ABIs."
    workingDir = coreDir
    commandLine(
        "cargo", "ndk",
        "-t", "arm64-v8a",
        "-t", "x86_64",
        "-o", jniLibsDir.get().asFile.absolutePath,
        "build",
    )
}

val generateUniffiBindings by tasks.registering(Exec::class) {
    description = "Generates the UniFFI Kotlin bindings from the built cdylib."
    dependsOn(cargoNdkBuild)
    workingDir = coreDir
    commandLine(
        "cargo", "run", "--bin", "uniffi-bindgen", "--",
        "generate",
        "--library", "target/aarch64-linux-android/debug/libpigeon_mobile_core.so",
        "--language", "kotlin",
        "--out-dir", uniffiDir.get().asFile.absolutePath,
    )
}

android {
    namespace = "com.pigeon.mobile"
    compileSdk = 34

    defaultConfig {
        applicationId = "com.pigeon.mobile"
        minSdk = 24
        targetSdk = 34
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
    kotlinOptions {
        jvmTarget = "17"
    }
    buildFeatures {
        compose = true
    }

    // Pick up the cargo-ndk .so output and the generated UniFFI Kotlin.
    sourceSets["main"].jniLibs.srcDir(jniLibsDir)
    sourceSets["main"].java.srcDir(uniffiDir)
}

tasks.named("preBuild") {
    dependsOn(generateUniffiBindings)
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