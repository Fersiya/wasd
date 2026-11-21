import java.io.File
import java.util.Properties

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

// версия берётся из ../version.properties (в CI её подставляет workflow из Cargo.toml)
val versionProps = Properties().apply {
    val f = File(rootDir, "version.properties")
    if (f.exists()) f.inputStream().use { load(it) }
}
val appVersionName = versionProps.getProperty("VERSION_NAME", "0.0.0")
val appVersionCode = versionProps.getProperty("VERSION_CODE", "1").toInt()

android {
    namespace = "app.nyxar"
    compileSdk = 36

    defaultConfig {
        applicationId = "app.nyxar"
        minSdk = 24
        targetSdk = 36
        versionCode = appVersionCode
        versionName = appVersionName
        // libbox собирается только под arm64 (почти все современные телефоны)
        ndk { abiFilters += listOf("arm64-v8a") }
    }

    buildFeatures { buildConfig = true }

    signingConfigs {
        create("release") {
            val ks = System.getenv("NYXAR_KEYSTORE")
            if (ks != null && File(ks).exists()) {
                storeFile = File(ks)
                storePassword = System.getenv("NYXAR_KEYSTORE_PASSWORD")
                keyAlias = System.getenv("NYXAR_KEY_ALIAS") ?: "nyxar"
                keyPassword = System.getenv("NYXAR_KEY_PASSWORD") ?: System.getenv("NYXAR_KEYSTORE_PASSWORD")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            if (System.getenv("NYXAR_KEYSTORE") != null) {
                signingConfig = signingConfigs.getByName("release")
            }
        }
    }

    packaging {
        jniLibs {
            useLegacyPackaging = true
            // libxray.so это Go-программа, её нельзя обрабатывать как обычную библиотеку
            keepDebugSymbols += "**/libxray.so"
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

kotlin {
    compilerOptions {
        jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17)
    }
}

dependencies {
    implementation(files("libs/libbox.aar"))
}
