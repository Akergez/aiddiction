// The Gradle project that packages Aiddiction for Android. The
// application is the Rust library; see app/build.gradle.kts and README.md.

buildscript {
    repositories {
        google()
        mavenCentral()
    }
    dependencies {
        classpath("com.android.tools.build:gradle:9.1.0")
    }
}

tasks.register("clean", Delete::class) {
    delete(rootProject.layout.buildDirectory)
}
