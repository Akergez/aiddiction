pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
        // The Java half of rustls-platform-verifier, which the client's
        // sockets check certificates with: rustls publishes it as a Maven
        // repository on a branch of its own (its README's Gradle setup).
        maven {
            url = uri("https://github.com/rustls/rustls-platform-verifier/raw/maven-archive/android-release-support/maven/")
        }
        // gpui-android-glue's Java half. GitHub Packages asks for a token even
        // to read: GITHUB_TOKEN on CI, `gpr.user`/`gpr.key` in
        // ~/.gradle/gradle.properties at a desk.
        maven {
            url = uri("https://maven.pkg.github.com/akergez/gpui-android-glue")
            credentials {
                username = providers.environmentVariable("GITHUB_ACTOR").orNull
                    ?: providers.gradleProperty("gpr.user").orNull
                password = providers.environmentVariable("GITHUB_TOKEN").orNull
                    ?: providers.gradleProperty("gpr.key").orNull
            }
        }
    }
}

rootProject.name = "Aiddiction"
include(":app")

// `-PglueLocal`: a checkout of gpui-android-glue beside this repository
// stands in for the published library, to work on the two together.
if (providers.gradleProperty("glueLocal").isPresent) {
    includeBuild("../../gpui-android-glue/android")
}
