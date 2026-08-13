package org.ratspeak.android

import android.Manifest
import android.app.NotificationManager
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.PowerManager
import android.provider.Settings
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import androidx.core.net.toUri
import java.lang.ref.WeakReference
import java.util.concurrent.atomic.AtomicLong

/**
 * Narrow process-native boundary for platform state that must not depend on a
 * live WebView. Rust owns protocol/application state; Android owns OS handles.
 */
object RatspeakNativeBridge {
    const val USB_ATTACHED = 1
    const val USB_DETACHED = 2
    const val USB_PERMISSION = 3
    const val USB_SNAPSHOT = 4

    const val BLE_CONNECTING = 0
    const val BLE_RECONNECTING = 1
    const val BLE_LISTENER_READY = 2
    const val BLE_CONNECTED = 3
    const val BLE_FAILED = 4
    const val BLE_DISABLED = 5

    private val lock = Any()
    private var contextRef = WeakReference<Context>(null)
    private val platformSequence = AtomicLong(0)

    @JvmStatic
    fun initialize(context: Context) {
        synchronized(lock) {
            contextRef = WeakReference(context.applicationContext)
        }
    }

    @JvmStatic
    fun replayPlatformState() {
        RatspeakPlatformSupervisor.replay()
    }

    private fun context(): Context? = synchronized(lock) { contextRef.get() }

    @JvmStatic
    fun startOrReplaceBleRnode(
        address: String,
        localPort: Int,
        operationToken: String,
        installedGeneration: Long,
    ): Boolean {
        val context = context() ?: return false
        return RatspeakBleRnodeSupervisor.startOrReplace(
            context,
            address,
            localPort,
            operationToken,
            installedGeneration,
        )
    }

    @JvmStatic
    fun disconnectBleRnode(operationToken: String?, installedGeneration: Long): Boolean {
        return RatspeakBleRnodeSupervisor.disconnect(operationToken, installedGeneration)
    }

    internal fun publishNetworkType(networkType: String) {
        val sequence = platformSequence.incrementAndGet()
        try {
            nativeSetNetworkType(networkType, sequence)
        } catch (error: UnsatisfiedLinkError) {
            Log.d("RatspeakNative", "network bridge unavailable: ${error.message}")
        }
    }

    internal fun publishUsbDevice(
        action: Int,
        deviceName: String?,
        vendorId: Int,
        productId: Int,
        serial: String?,
        permission: Boolean,
    ) {
        val sequence = platformSequence.incrementAndGet()
        try {
            nativeUsbDeviceEvent(
                action,
                deviceName,
                vendorId,
                productId,
                serial,
                permission,
                sequence,
            )
        } catch (error: UnsatisfiedLinkError) {
            Log.d("RatspeakNative", "USB bridge unavailable: ${error.message}")
        }
    }

    internal fun publishEmptyUsbInventory() {
        publishUsbDevice(USB_SNAPSHOT, null, 0, 0, null, false)
    }

    internal fun publishBleState(
        operationToken: String,
        installedGeneration: Long,
        state: Int,
        localPort: Int,
        errorCode: String?,
    ) {
        try {
            nativeBleRnodeState(
                operationToken,
                installedGeneration,
                state,
                localPort,
                errorCode,
            )
        } catch (error: UnsatisfiedLinkError) {
            Log.d("RatspeakNative", "BLE bridge unavailable: ${error.message}")
        }
    }

    @JvmStatic
    fun notificationAuthorizationStatus(): String {
        val context = context() ?: return "unavailable"
        val permissionRequired = Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU
        val permissionGranted = !permissionRequired || ContextCompat.checkSelfPermission(
                context,
                Manifest.permission.POST_NOTIFICATIONS,
            ) == PackageManager.PERMISSION_GRANTED
        val channelEnabled = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val manager = context.getSystemService(NotificationManager::class.java)
            manager?.getNotificationChannel(RatspeakService.MSG_CHANNEL_ID)?.importance !=
                NotificationManager.IMPORTANCE_NONE
        } else true
        return RatspeakMobilePolicy.notificationAuthorizationState(
            permissionRequired,
            permissionGranted,
            NotificationManagerCompat.from(context).areNotificationsEnabled(),
            channelEnabled,
        )
    }

    @JvmStatic
    fun openNotificationSettings(): Boolean {
        val context = context() ?: return false
        return try {
            val intent = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS).apply {
                    putExtra(Settings.EXTRA_APP_PACKAGE, context.packageName)
                }
            } else {
                Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS).apply {
                    data = "package:${context.packageName}".toUri()
                }
            }.apply {
                addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            }
            context.startActivity(intent)
            true
        } catch (_: Throwable) {
            false
        }
    }

    @JvmStatic
    fun batteryOptimizationStatus(): String {
        val context = context() ?: return "unavailable"
        val manager = context.getSystemService(PowerManager::class.java) ?: return "unavailable"
        return RatspeakMobilePolicy.batteryOptimizationState(
            manager.isIgnoringBatteryOptimizations(context.packageName),
        )
    }

    /** Must only be called from an explicit, visible user action. */
    @JvmStatic
    fun requestBatteryOptimizationExemption(): Boolean {
        val context = context() ?: return false
        return try {
            // Open the system-managed allowlist only from the explicit
            // Settings action. Ratspeak does not bypass Play policy with a
            // package-targeted exemption prompt.
            val intent = Intent(Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS).apply {
                addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            }
            context.startActivity(intent)
            true
        } catch (_: Throwable) {
            false
        }
    }

    private external fun nativeSetNetworkType(networkType: String, sequence: Long)
    private external fun nativeUsbDeviceEvent(
        action: Int,
        deviceName: String?,
        vendorId: Int,
        productId: Int,
        serial: String?,
        permission: Boolean,
        sequence: Long,
    )
    private external fun nativeBleRnodeState(
        operationToken: String,
        installedGeneration: Long,
        state: Int,
        localPort: Int,
        errorCode: String?,
    )
}
