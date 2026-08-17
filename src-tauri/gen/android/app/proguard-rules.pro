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

# Ratspeak Android runtime boundaries. Rust resolves these classes and members
# by their literal JVM names, while several classes also call exported native
# methods by JNI convention. Keep the complete boundary shape under R8.
-keep class org.ratspeak.android.MainActivity { *; }
-keep class org.ratspeak.android.RatspeakService { *; }
-keep class org.ratspeak.android.RatspeakNativeBridge { *; }
-keep class org.ratspeak.android.RatspeakPlatformSupervisor { *; }
-keep class org.ratspeak.android.RatspeakBleServer { *; }
-keep class org.ratspeak.android.RatspeakGattCallback { *; }
-keep class org.ratspeak.android.RatspeakBlePeerClient { *; }
-keep class org.ratspeak.android.RatspeakBlePeerClient$Companion { *; }
-keep class org.ratspeak.android.RatspeakBleAvailability { *; }
-keep class org.ratspeak.android.RatspeakAdvertiseCallback { *; }

# LXST voice audio bridge (called from Rust JNI by class and method name)
-keep class org.ratspeak.android.RatspeakVoiceAudio { *; }
-keep class org.ratspeak.android.RatspeakCallAudio { *; }
-keep class org.ratspeak.android.RatspeakVoiceMemoAudio { *; }

# BLE permission bridge (JavaScript interface)
-keepclassmembers class org.ratspeak.android.MainActivity$BlePermissionBridge {
    @android.webkit.JavascriptInterface <methods>;
}
