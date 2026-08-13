package org.ratspeak.android

import java.lang.ref.WeakReference

/** UI observers are optional and never own native radio resources. */
object RatspeakAndroidObservers {
    private val lock = Any()
    private var activity = WeakReference<MainActivity>(null)

    fun attach(candidate: MainActivity) {
        synchronized(lock) { activity = WeakReference(candidate) }
    }

    fun detach(candidate: MainActivity) {
        synchronized(lock) {
            if (activity.get() === candidate) activity.clear()
        }
    }

    fun bleProgress(token: String, generation: Long, phase: String) {
        synchronized(lock) { activity.get() }?.onNativeBleProgress(token, generation, phase)
    }

    fun usbPermission(deviceName: String, granted: Boolean, error: String?) {
        synchronized(lock) { activity.get() }?.onNativeUsbPermission(deviceName, granted, error)
    }

    fun usbSelectorPermission(granted: Boolean, errorCode: String?) {
        synchronized(lock) { activity.get() }?.onNativeUsbSelectorPermission(granted, errorCode)
    }
}
