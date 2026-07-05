import java.util.Properties
import org.gradle.api.GradleException

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("rust")
}

val tauriProperties = Properties().apply {
    val propFile = file("tauri.properties")
    if (propFile.exists()) {
        propFile.inputStream().use { load(it) }
    }
}

val signingProperties = Properties().apply {
    val propFile = rootProject.file("keystore.properties")
    if (propFile.exists()) {
        propFile.inputStream().use { load(it) }
    }
}
val hasReleaseSigning = signingProperties.containsKey("storeFile")

fun normalizeDisplayVersion(value: String?): String? {
    val version = value
        ?.trim()
        ?.removePrefix("v")
        ?.trim()
        ?: return null
    return if (version.firstOrNull()?.isDigit() == true) version else null
}

fun ratspeakDisplayVersionName(): String {
    val versionFile = rootProject.file("../../../VERSION")
    val fileVersion = if (versionFile.exists()) versionFile.readText() else null
    return normalizeDisplayVersion(System.getenv("RATSPEAK_DISPLAY_VERSION"))
        ?: normalizeDisplayVersion(System.getenv("GITHUB_REF_NAME"))
        ?: normalizeDisplayVersion(fileVersion)
        ?: tauriProperties.getProperty("tauri.android.versionName", "1.0")
}

android {
    compileSdk = 36
    namespace = "org.ratspeak.android"
    defaultConfig {
        // Tauri production bundles load from the asset protocol, not HTTP.
        manifestPlaceholders["usesCleartextTraffic"] = "false"
        applicationId = "org.ratspeak.android"
        minSdk = 24
        targetSdk = 36
        versionCode = tauriProperties.getProperty("tauri.android.versionCode", "1").toInt()
        versionName = ratspeakDisplayVersionName()
    }
    if (hasReleaseSigning) {
        signingConfigs {
            create("release") {
                storeFile = rootProject.file(signingProperties.getProperty("storeFile"))
                storePassword = signingProperties.getProperty("storePassword")
                keyAlias = signingProperties.getProperty("keyAlias")
                keyPassword = signingProperties.getProperty("keyPassword")
            }
        }
    }
    buildTypes {
        getByName("debug") {
            manifestPlaceholders["usesCleartextTraffic"] = "false"
            isDebuggable = true
            isJniDebuggable = true
            isMinifyEnabled = false
            packaging {
                jniLibs.keepDebugSymbols.clear()
            }
        }
        getByName("release") {
            isMinifyEnabled = true
            proguardFiles(
                *fileTree(".") { include("**/*.pro") }
                    .plus(getDefaultProguardFile("proguard-android-optimize.txt"))
                    .toList().toTypedArray()
            )
            if (hasReleaseSigning) {
                signingConfig = signingConfigs.getByName("release")
            }
        }
    }
    kotlinOptions {
        jvmTarget = "1.8"
    }
    buildFeatures {
        buildConfig = true
    }
}

fun patchTauriGeneratedLoggerFile() {
    val logger = file("src/main/java/org/ratspeak/android/generated/Logger.kt")
    if (!logger.exists()) {
        throw GradleException("Tauri generated Logger.kt is missing")
    }

    val source = logger.readText()
    val patched = source.replace(
        "return BuildConfig.DEBUG",
        "return RatspeakDiagnostics.enabled()"
    )
    if (patched != source) {
        logger.writeText(patched)
    }
    if (!patched.contains("return RatspeakDiagnostics.enabled()")) {
        throw GradleException("Tauri Logger.kt is not privacy-gated")
    }

    val rustWebView = file("src/main/java/org/ratspeak/android/generated/RustWebView.kt")
    if (!rustWebView.exists()) {
        throw GradleException("Tauri generated RustWebView.kt is missing")
    }
    val rustWebViewSource = rustWebView.readText()
    val rustWebViewPatched = rustWebViewSource.replace(
        "@file:Suppress(\"unused\", \"SetJavaScriptEnabled\")",
        "@file:Suppress(\"unused\", \"SetJavaScriptEnabled\", \"DEPRECATION\")"
    )
    if (rustWebViewPatched != rustWebViewSource) {
        rustWebView.writeText(rustWebViewPatched)
    }
    if (!rustWebViewPatched.contains("@file:Suppress(\"unused\", \"SetJavaScriptEnabled\", \"DEPRECATION\")")) {
        throw GradleException("Tauri RustWebView.kt deprecation warning is not suppressed")
    }

    val rustWebChromeClient = file("src/main/java/org/ratspeak/android/generated/RustWebChromeClient.kt")
    if (!rustWebChromeClient.exists()) {
        throw GradleException("Tauri generated RustWebChromeClient.kt is missing")
    }
    val rustWebChromeClientSource = rustWebChromeClient.readText()
    val geolocationMarker =
        "    Logger.debug(\"onGeolocationPermissionsShowPrompt: DOING IT HERE FOR ORIGIN: ${'$'}origin\")\n" +
            "    val geoPermissions ="
    val geolocationPatch =
        "    Logger.debug(\"onGeolocationPermissionsShowPrompt: DOING IT HERE FOR ORIGIN: ${'$'}origin\")\n" +
            "    val coarseLocationPermission = arrayOf(Manifest.permission.ACCESS_COARSE_LOCATION)\n" +
            "    if (PermissionHelper.hasPermissions(activity, coarseLocationPermission)) {\n" +
            "      callback.invoke(origin, true, false)\n" +
            "      Logger.debug(\"onGeolocationPermissionsShowPrompt: coarse permission already granted\")\n" +
            "      return\n" +
            "    }\n" +
            "    val geoPermissions ="
    val rustWebChromeClientPatched =
        if (rustWebChromeClientSource.contains("onGeolocationPermissionsShowPrompt: coarse permission already granted")) {
            rustWebChromeClientSource
        } else {
            rustWebChromeClientSource.replace(geolocationMarker, geolocationPatch)
        }
    if (rustWebChromeClientPatched != rustWebChromeClientSource) {
        rustWebChromeClient.writeText(rustWebChromeClientPatched)
    }
    if (!rustWebChromeClientPatched.contains("onGeolocationPermissionsShowPrompt: coarse permission already granted")) {
        throw GradleException("Tauri RustWebChromeClient.kt coarse geolocation permission patch is missing")
    }

    val wryActivity = file("src/main/java/org/ratspeak/android/generated/WryActivity.kt")
    if (!wryActivity.exists()) {
        throw GradleException("Tauri generated WryActivity.kt is missing")
    }
    val wryActivitySource = wryActivity.readText()
    val wryActivityPatched = if (wryActivitySource.contains("@file:Suppress(\"DEPRECATION\")")) {
        wryActivitySource
    } else {
        wryActivitySource.replace(
            "// SPDX-License-Identifier: MIT\n\npackage org.ratspeak.android",
            "// SPDX-License-Identifier: MIT\n\n@file:Suppress(\"DEPRECATION\")\n\npackage org.ratspeak.android"
        )
    }
    if (wryActivityPatched != wryActivitySource) {
        wryActivity.writeText(wryActivityPatched)
    }
    if (!wryActivityPatched.contains("@file:Suppress(\"DEPRECATION\")")) {
        throw GradleException("Tauri WryActivity.kt deprecation warning is not suppressed")
    }
}

val patchTauriGeneratedLogger = tasks.register("patchTauriGeneratedLogger") {
    doLast { patchTauriGeneratedLoggerFile() }
}

tasks.matching { it.name.startsWith("compile") && it.name.endsWith("Kotlin") }.configureEach {
    dependsOn(patchTauriGeneratedLogger)
    doFirst { patchTauriGeneratedLoggerFile() }
    outputs.upToDateWhen { false }
}

tasks.matching { it.name.startsWith("rustBuild") }.configureEach {
    finalizedBy(patchTauriGeneratedLogger)
}

tasks.matching { it.name.startsWith("assemble") || it.name.startsWith("bundle") }.configureEach {
    doLast { patchTauriGeneratedLoggerFile() }
}

rust {
    rootDirRel = "../../../"
}

dependencies {
    implementation("androidx.webkit:webkit:1.14.0")
    implementation("androidx.appcompat:appcompat:1.7.1")
    implementation("androidx.activity:activity-ktx:1.10.1")
    implementation("com.google.android.material:material:1.12.0")
    testImplementation("junit:junit:4.13.2")
    androidTestImplementation("androidx.test.ext:junit:1.1.4")
    androidTestImplementation("androidx.test.espresso:espresso-core:3.5.0")
}

apply(from = "tauri.build.gradle.kts")
