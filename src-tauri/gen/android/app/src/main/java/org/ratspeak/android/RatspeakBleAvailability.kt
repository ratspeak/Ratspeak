package org.ratspeak.android

import android.Manifest
import android.annotation.SuppressLint
import android.bluetooth.BluetoothManager
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import org.json.JSONArray
import org.json.JSONObject

object RatspeakBleAvailability {
    private fun requiredPermissions(): Array<String> {
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            arrayOf(
                Manifest.permission.BLUETOOTH_SCAN,
                Manifest.permission.BLUETOOTH_CONNECT,
                Manifest.permission.BLUETOOTH_ADVERTISE
            )
        } else {
            arrayOf(Manifest.permission.ACCESS_FINE_LOCATION)
        }
    }

    private fun permissionLabel(permission: String): String {
        return when (permission) {
            Manifest.permission.BLUETOOTH_SCAN -> "Bluetooth scan"
            Manifest.permission.BLUETOOTH_CONNECT -> "Bluetooth connect"
            Manifest.permission.BLUETOOTH_ADVERTISE -> "Bluetooth advertise"
            Manifest.permission.ACCESS_FINE_LOCATION -> "Location"
            else -> permission.substringAfterLast('.')
        }
    }

    @JvmStatic
    @SuppressLint("MissingPermission")
    fun check(context: Context): String {
        return try {
            checkInner(context.applicationContext ?: context).toString()
        } catch (t: Throwable) {
            // This probe is advisory. If a platform quirk or JNI/reflection
            // issue breaks it, do not brick the BLE Peer path that can still
            // request permissions and fail with a concrete runtime error.
            JSONObject().apply {
                put("available", true)
                put("missing", JSONArray())
                put("missing_permissions", JSONArray())
                put("permissions_granted", false)
                put("permission_required", false)
                put("ble_supported", JSONObject.NULL)
                put("bluetooth_enabled", JSONObject.NULL)
                put("scanner_available", JSONObject.NULL)
                put("advertiser_available", JSONObject.NULL)
                put("probe_failed", true)
                put("error", t.javaClass.name + ": " + (t.message ?: "unknown"))
            }.toString()
        }
    }

    @SuppressLint("MissingPermission")
    private fun checkInner(context: Context): JSONObject {
        val missing = JSONArray()
        val missingPermissions = JSONArray()
        val pm = context.packageManager
        val bleSupported = pm.hasSystemFeature(PackageManager.FEATURE_BLUETOOTH_LE)

        if (!bleSupported) {
            missing.put("Bluetooth LE is not supported on this device")
        }

        for (permission in requiredPermissions()) {
            if (context.checkSelfPermission(permission) != PackageManager.PERMISSION_GRANTED) {
                missingPermissions.put(permissionLabel(permission))
            }
        }
        val permissionsGranted = missingPermissions.length() == 0
        if (!permissionsGranted) {
            missing.put("Bluetooth permissions not granted")
        }

        var adapterMissing = false
        val manager = try {
            context.getSystemService(BluetoothManager::class.java)
        } catch (_: Throwable) {
            null
        }
        val adapter = if (permissionsGranted) {
            try {
                manager?.adapter
            } catch (_: SecurityException) {
                missing.put("Bluetooth connect permission denied")
                null
            }
        } else {
            null
        }
        if (permissionsGranted && (manager == null || adapter == null)) {
            adapterMissing = true
            missing.put("No Bluetooth adapter found")
        }

        var bluetoothEnabled: Boolean? = null
        if (adapter != null && permissionsGranted) {
            bluetoothEnabled = try {
                adapter.isEnabled
            } catch (_: SecurityException) {
                missing.put("Bluetooth connect permission denied")
                false
            }
            if (bluetoothEnabled == false) {
                missing.put("Bluetooth is turned off")
            }
        }

        var scannerAvailable: Boolean? = null
        var advertiserAvailable: Boolean? = null
        if (adapter != null && permissionsGranted && bluetoothEnabled == true) {
            scannerAvailable = try {
                adapter.bluetoothLeScanner != null
            } catch (_: SecurityException) {
                false
            }
            advertiserAvailable = try {
                adapter.bluetoothLeAdvertiser != null
            } catch (_: SecurityException) {
                false
            }
            if (scannerAvailable == false) {
                missing.put("Bluetooth scanner unavailable")
            }
        }

        val permissionRequired = !permissionsGranted
        val available = when {
            !bleSupported -> false
            adapterMissing -> false
            permissionRequired -> true
            else -> missing.length() == 0
        }

        return JSONObject().apply {
            put("available", available)
            put("missing", missing)
            put("missing_permissions", missingPermissions)
            put("permissions_granted", permissionsGranted)
            put("permission_required", permissionRequired)
            put("ble_supported", bleSupported)
            put("bluetooth_enabled", bluetoothEnabled ?: JSONObject.NULL)
            put("scanner_available", scannerAvailable ?: JSONObject.NULL)
            put("advertiser_available", advertiserAvailable ?: JSONObject.NULL)
        }
    }
}
