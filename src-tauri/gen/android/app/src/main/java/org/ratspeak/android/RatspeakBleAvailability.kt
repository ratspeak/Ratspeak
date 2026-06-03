package org.ratspeak.android

import android.Manifest
import android.annotation.SuppressLint
import android.bluetooth.BluetoothManager
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.content.ContextCompat
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
        val missing = JSONArray()
        val missingPermissions = JSONArray()
        val pm = context.packageManager
        val bleSupported = pm.hasSystemFeature(PackageManager.FEATURE_BLUETOOTH_LE)

        if (!bleSupported) {
            missing.put("Bluetooth LE is not supported on this device")
        }

        for (permission in requiredPermissions()) {
            if (ContextCompat.checkSelfPermission(context, permission) !=
                PackageManager.PERMISSION_GRANTED) {
                missingPermissions.put(permissionLabel(permission))
            }
        }
        val permissionsGranted = missingPermissions.length() == 0
        if (!permissionsGranted) {
            missing.put("Bluetooth permissions not granted")
        }

        val manager = context.getSystemService(BluetoothManager::class.java)
        val adapter = manager?.adapter
        if (manager == null || adapter == null) {
            missing.put("No Bluetooth adapter found")
        }

        var bluetoothEnabled = false
        if (adapter != null && permissionsGranted) {
            bluetoothEnabled = try {
                adapter.isEnabled
            } catch (_: SecurityException) {
                missing.put("Bluetooth connect permission denied")
                false
            }
            if (!bluetoothEnabled) {
                missing.put("Bluetooth is turned off")
            }
        }

        var scannerAvailable = false
        var advertiserAvailable = false
        if (adapter != null && permissionsGranted && bluetoothEnabled) {
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
            if (!scannerAvailable) {
                missing.put("Bluetooth scanner unavailable")
            }
        }

        return JSONObject().apply {
            put("available", missing.length() == 0)
            put("missing", missing)
            put("missing_permissions", missingPermissions)
            put("permissions_granted", permissionsGranted)
            put("ble_supported", bleSupported)
            put("bluetooth_enabled", bluetoothEnabled)
            put("scanner_available", scannerAvailable)
            put("advertiser_available", advertiserAvailable)
        }.toString()
    }
}
