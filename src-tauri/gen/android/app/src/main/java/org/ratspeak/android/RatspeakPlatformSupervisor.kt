package org.ratspeak.android

import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.hardware.usb.UsbDevice
import android.hardware.usb.UsbManager
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.os.Build
import android.annotation.SuppressLint
import androidx.core.content.ContextCompat

/** Service-owned Android network and USB observation. */
@SuppressLint("StaticFieldLeak") // Service/application ownership is explicit and released in stop().
object RatspeakPlatformSupervisor {
    private const val USB_PERMISSION_ACTION = "org.ratspeak.android.USB_PERMISSION"
    private const val USB_SELECTOR_PERMISSION_ACTION =
        "org.ratspeak.android.USB_SELECTOR_PERMISSION"
    private val lock = Any()
    private var context: Context? = null
    private var service: RatspeakService? = null
    private var connectivityManager: ConnectivityManager? = null
    private var networkCallback: ConnectivityManager.NetworkCallback? = null
    private var usbSystemReceiver: BroadcastReceiver? = null
    private var usbPermissionReceiver: BroadcastReceiver? = null
    private var lastNetworkType: String? = null
    private var pendingUsbSelector: PendingUsbSelector? = null
    private var pendingLegacyUsbRecovery: String? = null
    private var nextUsbSelectorSequence = 0L

    private data class PendingUsbSelector(
        val deviceName: String,
        val identity: RatspeakMobilePolicy.UsbIdentity,
        val sequence: Long,
    )

    fun start(service: RatspeakService) {
        synchronized(lock) {
            if (this.service === service) return
            stopLocked()
            this.context = service.applicationContext
            this.service = service
            RatspeakNativeBridge.initialize(service.applicationContext)
            registerNetworkLocked()
            registerUsbLocked()
            seedUsbLocked()
        }
    }

    fun stop(service: RatspeakService) {
        synchronized(lock) {
            if (this.service !== service) return
            stopLocked()
        }
    }

    fun replay() {
        synchronized(lock) {
            lastNetworkType?.let(RatspeakNativeBridge::publishNetworkType)
            seedUsbLocked()
        }
    }

    @JvmStatic
    fun requestUsbPermission(deviceName: String) {
        requestUsbPermissionByPath(deviceName, false)
    }

    @JvmStatic
    fun requestUsbPermissionForLegacyPath(deviceName: String) {
        requestUsbPermissionByPath(deviceName, true)
    }

    private fun requestUsbPermissionByPath(deviceName: String, recovery: Boolean) {
        val appContext: Context
        val manager: UsbManager
        val device: UsbDevice
        synchronized(lock) {
            appContext = context ?: run {
                RatspeakAndroidObservers.usbPermission(deviceName, false, "USB service unavailable")
                if (recovery) RatspeakAndroidObservers.usbSelectorPermission(false, "service_unavailable")
                return
            }
            manager = appContext.getSystemService(Context.USB_SERVICE) as? UsbManager ?: run {
                RatspeakAndroidObservers.usbPermission(deviceName, false, "USB service unavailable")
                if (recovery) RatspeakAndroidObservers.usbSelectorPermission(false, "service_unavailable")
                return
            }
            device = manager.deviceList[deviceName] ?: run {
                RatspeakAndroidObservers.usbPermission(deviceName, false, "USB device is not connected")
                if (recovery) RatspeakAndroidObservers.usbSelectorPermission(false, "no_match")
                return
            }
            if (manager.hasPermission(device)) {
                publishUsb(RatspeakNativeBridge.USB_PERMISSION, device, true)
                RatspeakAndroidObservers.usbPermission(deviceName, true, null)
                if (recovery) RatspeakAndroidObservers.usbSelectorPermission(true, null)
                return
            }
            if (recovery) pendingLegacyUsbRecovery = deviceName
        }
        val intent = Intent(USB_PERMISSION_ACTION).setPackage(appContext.packageName)
        val pending = PendingIntent.getBroadcast(
            appContext,
            0,
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        try {
            manager.requestPermission(device, pending)
        } catch (_: Throwable) {
            synchronized(lock) {
                if (recovery && pendingLegacyUsbRecovery == deviceName) {
                    pendingLegacyUsbRecovery = null
                }
            }
            RatspeakAndroidObservers.usbPermission(deviceName, false, "USB permission request failed")
            if (recovery) RatspeakAndroidObservers.usbSelectorPermission(false, "request_failed")
        }
    }

    /** Visible-user recovery action using persisted identity, never a stale Android path. */
    @JvmStatic
    fun requestUsbPermissionForSelector(vendorId: Int, productId: Int, serial: String?) {
        if (vendorId !in 0..0xffff || productId !in 0..0xffff ||
            serial != null && (serial.length > 256 || serial.any { it.isISOControl() })
        ) {
            RatspeakAndroidObservers.usbSelectorPermission(false, "invalid_selector")
            return
        }
        val appContext: Context
        val manager: UsbManager
        val selected: UsbDevice
        val requestSequence: Long
        val wanted = RatspeakMobilePolicy.UsbIdentity(
            vendorId,
            productId,
            serial?.trim()?.takeIf { it.isNotEmpty() },
        )
        synchronized(lock) {
            appContext = context ?: run {
                RatspeakAndroidObservers.usbSelectorPermission(false, "service_unavailable")
                return
            }
            manager = usbManager() ?: run {
                RatspeakAndroidObservers.usbSelectorPermission(false, "service_unavailable")
                return
            }
            // A new visible recovery action supersedes any older permission
            // dialog callback, even when this request resolves immediately.
            pendingUsbSelector = null
            val devices = manager.deviceList.values.sortedBy { it.deviceName }
            val candidates = devices.map { device ->
                RatspeakMobilePolicy.UsbIdentity(
                    device.vendorId,
                    device.productId,
                    safeSerial(device, manager.hasPermission(device)),
                )
            }
            val plan = RatspeakMobilePolicy.usbPermissionPlan(wanted, candidates)
            val index = plan.candidateIndex
            if (index == null) {
                RatspeakAndroidObservers.usbSelectorPermission(
                    false,
                    plan.errorCode ?: "no_match",
                )
                return
            }
            selected = devices[index]
            if (manager.hasPermission(selected)) {
                if (!selectorMatchesAfterPermission(wanted, selected)) {
                    RatspeakAndroidObservers.usbSelectorPermission(false, "selector_mismatch")
                    return
                }
                publishUsb(RatspeakNativeBridge.USB_PERMISSION, selected, true)
                RatspeakAndroidObservers.usbSelectorPermission(true, null)
                return
            }
            nextUsbSelectorSequence = if (nextUsbSelectorSequence == Long.MAX_VALUE) {
                1L
            } else {
                nextUsbSelectorSequence + 1L
            }
            requestSequence = nextUsbSelectorSequence
            pendingUsbSelector = PendingUsbSelector(selected.deviceName, wanted, requestSequence)
        }
        val intent = Intent(USB_SELECTOR_PERMISSION_ACTION)
            .setPackage(appContext.packageName)
            .putExtra("selector_request", requestSequence)
        val pending = PendingIntent.getBroadcast(
            appContext,
            (requestSequence and 0x7fff_ffffL).toInt(),
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        try {
            manager.requestPermission(selected, pending)
        } catch (_: Throwable) {
            synchronized(lock) {
                if (pendingUsbSelector?.sequence == requestSequence) {
                    pendingUsbSelector = null
                }
            }
            RatspeakAndroidObservers.usbSelectorPermission(false, "request_failed")
        }
    }

    private fun registerNetworkLocked() {
        val appContext = context ?: return
        val manager = appContext.getSystemService(ConnectivityManager::class.java) ?: return
        connectivityManager = manager
        val callback = object : ConnectivityManager.NetworkCallback() {
            override fun onAvailable(network: Network) = publishNetwork(manager.getNetworkCapabilities(network))
            override fun onCapabilitiesChanged(network: Network, caps: NetworkCapabilities) = publishNetwork(caps)
            override fun onLost(network: Network) = publishNetwork(manager.activeNetwork?.let(manager::getNetworkCapabilities))
        }
        try {
            manager.registerDefaultNetworkCallback(callback)
            networkCallback = callback
            publishNetwork(manager.activeNetwork?.let(manager::getNetworkCapabilities))
        } catch (error: Throwable) {
            Log.w("RatspeakPlatform", "network callback unavailable: ${error.message}")
        }
    }

    private fun publishNetwork(capabilities: NetworkCapabilities?) {
        val type = RatspeakMobilePolicy.networkType(
            available = capabilities != null,
            wifi = capabilities?.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) == true,
            cellular = capabilities?.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR) == true,
            ethernet = capabilities?.hasTransport(NetworkCapabilities.TRANSPORT_ETHERNET) == true,
        )
        synchronized(lock) {
            if (lastNetworkType == type) return
            lastNetworkType = type
            service?.setMulticastNeeded(type == "wifi")
        }
        RatspeakNativeBridge.publishNetworkType(type)
    }

    private fun registerUsbLocked() {
        val appContext = context ?: return
        val systemReceiver = object : BroadcastReceiver() {
            override fun onReceive(context: Context, intent: Intent) {
                val device = usbDevice(intent) ?: return
                when (intent.action) {
                    UsbManager.ACTION_USB_DEVICE_ATTACHED -> publishUsb(
                        RatspeakNativeBridge.USB_ATTACHED,
                        device,
                        usbManager()?.hasPermission(device) == true,
                    )
                    UsbManager.ACTION_USB_DEVICE_DETACHED -> publishUsb(
                        RatspeakNativeBridge.USB_DETACHED,
                        device,
                        false,
                    )
                }
            }
        }
        val systemFilter = IntentFilter().apply {
            addAction(UsbManager.ACTION_USB_DEVICE_ATTACHED)
            addAction(UsbManager.ACTION_USB_DEVICE_DETACHED)
        }
        ContextCompat.registerReceiver(
            appContext,
            systemReceiver,
            systemFilter,
            // These are protected framework broadcasts. Some vendor USB
            // services are separate privileged packages, so NOT_EXPORTED can
            // incorrectly hide their delivery.
            ContextCompat.RECEIVER_EXPORTED,
        )
        usbSystemReceiver = systemReceiver

        val permissionReceiver = object : BroadcastReceiver() {
            override fun onReceive(context: Context, intent: Intent) {
                if ((intent.action != USB_PERMISSION_ACTION &&
                    intent.action != USB_SELECTOR_PERMISSION_ACTION) ||
                    (intent.`package` != null && intent.`package` != context.packageName)
                ) return
                val device = usbDevice(intent)
                if (device == null) {
                    if (intent.action == USB_SELECTOR_PERMISSION_ACTION) {
                        val requestSequence = intent.getLongExtra("selector_request", 0L)
                        val removed = synchronized(lock) {
                            if (pendingUsbSelector?.sequence == requestSequence) {
                                pendingUsbSelector = null
                                true
                            } else {
                                false
                            }
                        }
                        if (removed) {
                            RatspeakAndroidObservers.usbSelectorPermission(false, "request_failed")
                        }
                    }
                    return
                }
                val granted = intent.getBooleanExtra(UsbManager.EXTRA_PERMISSION_GRANTED, false)
                if (intent.action == USB_SELECTOR_PERMISSION_ACTION) {
                    val requestSequence = intent.getLongExtra("selector_request", 0L)
                    val selector = synchronized(lock) {
                        pendingUsbSelector?.takeIf {
                            it.deviceName == device.deviceName && it.sequence == requestSequence
                        }
                            .also { if (it != null) pendingUsbSelector = null }
                    } ?: return
                    val exact = granted && selectorMatchesAfterPermission(selector.identity, device)
                    publishUsb(RatspeakNativeBridge.USB_PERMISSION, device, exact)
                    RatspeakAndroidObservers.usbSelectorPermission(
                        exact,
                        when {
                            !granted -> "permission_denied"
                            !exact -> "selector_mismatch"
                            else -> null
                        },
                    )
                } else {
                    publishUsb(RatspeakNativeBridge.USB_PERMISSION, device, granted)
                    RatspeakAndroidObservers.usbPermission(device.deviceName, granted, null)
                    val legacyRecovery = synchronized(lock) {
                        if (pendingLegacyUsbRecovery == device.deviceName) {
                            pendingLegacyUsbRecovery = null
                            true
                        } else {
                            false
                        }
                    }
                    if (legacyRecovery) {
                        RatspeakAndroidObservers.usbSelectorPermission(
                            granted,
                            if (granted) null else "permission_denied",
                        )
                    }
                }
            }
        }
        val permissionFilter = IntentFilter().apply {
            addAction(USB_PERMISSION_ACTION)
            addAction(USB_SELECTOR_PERMISSION_ACTION)
        }
        ContextCompat.registerReceiver(
            appContext,
            permissionReceiver,
            permissionFilter,
            ContextCompat.RECEIVER_NOT_EXPORTED,
        )
        usbPermissionReceiver = permissionReceiver
    }

    private fun seedUsbLocked() {
        val manager = usbManager() ?: return
        val devices = manager.deviceList.values.sortedBy { it.deviceName }
        if (devices.isEmpty()) {
            RatspeakNativeBridge.publishEmptyUsbInventory()
            return
        }
        devices.forEach { device ->
            publishUsb(RatspeakNativeBridge.USB_SNAPSHOT, device, manager.hasPermission(device))
        }
    }

    private fun publishUsb(action: Int, device: UsbDevice, permission: Boolean) {
        RatspeakNativeBridge.publishUsbDevice(
            action,
            device.deviceName,
            device.vendorId,
            device.productId,
            safeSerial(device, permission),
            permission,
        )
    }

    private fun safeSerial(device: UsbDevice, permission: Boolean): String? {
        if (!permission) return null
        return try { device.serialNumber?.trim()?.takeIf { it.isNotEmpty() } } catch (_: Throwable) { null }
    }

    private fun selectorMatchesAfterPermission(
        wanted: RatspeakMobilePolicy.UsbIdentity,
        device: UsbDevice,
    ): Boolean {
        if (device.vendorId != wanted.vendorId || device.productId != wanted.productId) return false
        val wantedSerial = wanted.serial?.trim()?.takeIf { it.isNotEmpty() } ?: return true
        return safeSerial(device, permission = true) == wantedSerial
    }

    private fun usbManager(): UsbManager? = context?.getSystemService(Context.USB_SERVICE) as? UsbManager

    @Suppress("DEPRECATION")
    private fun usbDevice(intent: Intent): UsbDevice? {
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            intent.getParcelableExtra(UsbManager.EXTRA_DEVICE, UsbDevice::class.java)
        } else {
            intent.getParcelableExtra(UsbManager.EXTRA_DEVICE)
        }
    }

    private fun stopLocked() {
        networkCallback?.let { callback ->
            try { connectivityManager?.unregisterNetworkCallback(callback) } catch (_: Throwable) {}
        }
        usbSystemReceiver?.let { receiver ->
            try { context?.unregisterReceiver(receiver) } catch (_: Throwable) {}
        }
        usbPermissionReceiver?.let { receiver ->
            try { context?.unregisterReceiver(receiver) } catch (_: Throwable) {}
        }
        networkCallback = null
        connectivityManager = null
        usbSystemReceiver = null
        usbPermissionReceiver = null
        service = null
        context = null
        lastNetworkType = null
        pendingUsbSelector = null
        pendingLegacyUsbRecovery = null
    }
}
