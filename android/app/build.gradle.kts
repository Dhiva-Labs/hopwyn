import java.util.Properties

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
}

android {
    namespace = "app.hopwyn.android"
    compileSdk = 36

    defaultConfig {
        applicationId = "app.hopwyn.android"
        minSdk = 26
        targetSdk = 36
        versionCode = 3
        versionName = "0.3.0"
        ndk {
            abiFilters += listOf("arm64-v8a", "armeabi-v7a", "x86_64")
        }
    }

    // The Play upload key, named in android/keystore.properties (kept out of
    // git): storeFile=/path/to/upload.jks and keyAlias=upload. The password
    // comes from HOPWYN_KEY_PASSWORD, which scripts/build-android.sh asks for,
    // so it is never stored in a file. Without the properties file, release
    // builds are signed with the debug key (fine for sideloading, not Play).
    val keyProps = rootProject.file("keystore.properties")
    signingConfigs {
        if (keyProps.exists()) {
            val p = Properties().apply { keyProps.inputStream().use { load(it) } }
            create("upload") {
                storeFile = file(p.getProperty("storeFile"))
                val password = System.getenv("HOPWYN_KEY_PASSWORD") ?: p.getProperty("storePassword")
                storePassword = password
                keyAlias = p.getProperty("keyAlias")
                keyPassword = System.getenv("HOPWYN_KEY_PASSWORD") ?: p.getProperty("keyPassword") ?: password
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
            signingConfig = signingConfigs.findByName("upload") ?: signingConfigs.getByName("debug")
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    buildFeatures {
        compose = true
    }
}

kotlin {
    jvmToolchain(17)
}

dependencies {
    val composeBom = platform("androidx.compose:compose-bom:2025.09.00")
    implementation(composeBom)
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.material:material-icons-extended")
    implementation("androidx.activity:activity-compose:1.11.0")
    implementation("androidx.lifecycle:lifecycle-viewmodel-compose:2.9.4")
    implementation("androidx.core:core-ktx:1.17.0")
}
