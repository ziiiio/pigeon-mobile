plugins {
    id("com.android.application") version "9.2.1" apply false
    // AGP 9 has built-in Kotlin (bundles KGP 2.2.10) — the standalone
    // `org.jetbrains.kotlin.android` plugin is no longer applied. The Compose
    // compiler plugin stays separate and must match AGP's built-in Kotlin.
    id("org.jetbrains.kotlin.plugin.compose") version "2.2.10" apply false
}