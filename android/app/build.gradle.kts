// Packages the Rust library into an APK.
//
// Gradle does not build the library: `cargo ndk` does, into
// app/src/main/jniLibs/<abi>/, and that has to happen first. See
// android/README.md.

plugins {
    id("com.android.application")
}

// The version is the release's: CI hands over its tag, without the `v`. A
// build at the desk takes what the crates say.
val release: String = providers.environmentVariable("AIDDICTION_VERSION").orNull
    ?.takeIf { it.isNotEmpty() }
    ?: Regex("""\[workspace\.package\][^\[]*?\nversion = "([^"]+)"""")
        .find(rootDir.parentFile.resolve("Cargo.toml").readText())!!
        .groupValues[1]
// The number the system compares to tell a newer package from an older one.
val (major, minor, patch) = release.split(".").map { it.toInt() }

// The key a release is signed with, as build-aux/build-android.sh hands it
// over. Without one the package is signed with this machine's debug key: it
// installs, but not over a package signed with another key.
val keystore: String? = providers.environmentVariable("ANDROID_KEYSTORE").orNull
    ?.takeIf { it.isNotEmpty() }

android {
    namespace = "app.akergez.aiddiction"
    compileSdk = 36

    defaultConfig {
        applicationId = "app.akergez.aiddiction"
        minSdk = 26          // Vulkan 1.0 is mandatory from API 26
        targetSdk = 34
        versionCode = major * 1_000_000 + minor * 1_000 + patch
        versionName = release

        ndk {
            abiFilters += listOf("arm64-v8a")
        }

        // What NativeActivity loads: the cdylib's name.
        manifestPlaceholders["nativeLibraryName"] = "aiddiction"
    }

    signingConfigs {
        if (keystore != null) {
            create("release") {
                storeFile = file(keystore)
                storePassword = System.getenv("ANDROID_KEYSTORE_PASSWORD")
                keyAlias = System.getenv("ANDROID_KEY_ALIAS") ?: "aiddiction"
                // A PKCS#12 keystore has one password for itself and its keys.
                keyPassword = System.getenv("ANDROID_KEYSTORE_PASSWORD")
            }
        }
    }

    buildTypes {
        release {
            signingConfig = signingConfigs.findByName("release")
                ?: signingConfigs.getByName("debug")
            isMinifyEnabled = false
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro"
            )
        }
        debug {
            isDebuggable = true
            isJniDebuggable = true
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_1_8
        targetCompatibility = JavaVersion.VERSION_1_8
    }

    sourceSets {
        getByName("main") {
            jniLibs.srcDirs("src/main/jniLibs")
        }
    }

    // Strips the debug information out of the library on the way into the
    // package: a debug build of it is over a gigabyte.
    ndkVersion = "27.0.12077973"

    packaging {
        jniLibs {
            // gpui-mobile is a cdylib of its own as well, and `cargo ndk`
            // copies that next to ours. It is already linked into ours.
            excludes += "**/libgpui_mobile-*.so"
        }
    }

    lint {
        abortOnError = false
        checkReleaseBuilds = false
    }
}

dependencies {
    // What the Java classes taken from gpui-mobile use.
    implementation("androidx.core:core:1.12.0")
    implementation("androidx.core:core-splashscreen:1.0.1")
    implementation("androidx.biometric:biometric:1.1.0")
    implementation("androidx.media:media:1.7.1")
}
