# Add project specific ProGuard rules here.
# You can control the set of applied configuration files using the
# proguardFiles setting in build.gradle.
#
# For more details, see
#   http://developer.android.com/guide/developing/tools/proguard.html

# btleplug BLE Java bridge (JNI access from Rust via FindClass)
-keep class com.nonpolynomial.btleplug.android.impl.** { *; }

# Rust JNI utility classes (futures, streams, callbacks)
-keep class io.github.gedgygedgy.rust.** { *; }

# Ratspeak BLE helpers (called from Rust JNI)
-keep class org.ratspeak.android.MainActivity { *; }
-keep class org.ratspeak.android.RatspeakService { *; }
-keep class org.ratspeak.android.RatspeakBleServer { *; }
-keep class org.ratspeak.android.RatspeakGattCallback { *; }
-keep class org.ratspeak.android.RatspeakBlePeerClient { *; }
-keep class org.ratspeak.android.RatspeakBlePeerClient$Companion { *; }
-keep class org.ratspeak.android.RatspeakBleAvailability { *; }

# LXST voice audio bridge (called from Rust JNI by class and method name)
-keep class org.ratspeak.android.RatspeakVoiceAudio { *; }

# BLE permission bridge (JavaScript interface)
-keepclassmembers class org.ratspeak.android.MainActivity$BlePermissionBridge {
    @android.webkit.JavascriptInterface <methods>;
}
