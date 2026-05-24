package org.ratspeak.android

import android.Manifest
import android.annotation.SuppressLint
import android.app.Activity
import android.app.NotificationManager
import android.app.PendingIntent
import android.bluetooth.BluetoothDevice
import android.bluetooth.BluetoothManager
import android.bluetooth.le.BluetoothLeScanner
import android.bluetooth.le.ScanCallback
import android.bluetooth.le.ScanResult
import android.bluetooth.le.ScanSettings
import android.content.ActivityNotFoundException
import android.content.BroadcastReceiver
import android.content.ContentValues
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
import android.media.AudioAttributes
import android.media.AudioDeviceInfo
import android.media.AudioFocusRequest
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioTrack
import android.net.Uri
import android.hardware.usb.UsbDevice
import android.hardware.usb.UsbManager
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.ParcelUuid
import android.os.PowerManager
import android.provider.MediaStore
import android.provider.OpenableColumns
import android.util.Base64
import android.webkit.JavascriptInterface
import android.webkit.WebView
import androidx.activity.OnBackPressedCallback
import androidx.activity.result.ActivityResultLauncher
import androidx.activity.result.contract.ActivityResultContracts
import androidx.activity.enableEdgeToEdge
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.webkit.WebViewCompat
import org.json.JSONArray
import org.json.JSONObject
import java.io.ByteArrayOutputStream
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlin.math.PI
import kotlin.math.cos
import kotlin.math.pow
import kotlin.math.sin

class MainActivity : TauriActivity() {
    companion object {
        private const val BLE_PERMISSION_REQUEST_CODE = 1001
        private const val NOTIFICATION_PERMISSION_REQUEST_CODE = 1002
        private const val MEDIA_PERMISSION_REQUEST_CODE = 1003
        private const val USB_PERMISSION_ACTION = "org.ratspeak.android.USB_PERMISSION"
        private const val MAX_IDENTITY_IMPORT_BYTES = 1024 * 1024
        private const val CALL_RINGTONE_SAMPLE_RATE = 44100
        private const val CALL_RINGTONE_LOOP_MS = 3200L
        private const val CALL_RINGTONE_E5_HZ = 659.255114
        private const val CALL_RINGTONE_G5_HZ = 783.990872
        private const val CALL_RINGTONE_B5_HZ = 987.766603
        private const val CALL_RINGTONE_OUTGOING_VOLUME = 0.18
        private const val CALL_RINGTONE_INCOMING_VOLUME = 0.36
        private const val CALL_RINGTONE_INCOMING_GLIDE_CENTS = 7.0
        private const val CALL_RINGTONE_OUTGOING_GLIDE_CENTS = -4.0
        private const val CALL_RINGTONE_INCOMING_ATTACK_MS = 6L
        private const val CALL_RINGTONE_OUTGOING_ATTACK_MS = 9L
        private const val CALL_RINGTONE_INCOMING_RELEASE_MS = 52L
        private const val CALL_RINGTONE_OUTGOING_RELEASE_MS = 64L
        private val CALL_RINGTONE_INCOMING_START_MS = longArrayOf(0L, 150L, 300L, 780L, 920L, 1070L)
        private val CALL_RINGTONE_INCOMING_FREQ_HZ = doubleArrayOf(
            CALL_RINGTONE_E5_HZ,
            CALL_RINGTONE_G5_HZ,
            CALL_RINGTONE_B5_HZ,
            CALL_RINGTONE_B5_HZ,
            CALL_RINGTONE_G5_HZ,
            CALL_RINGTONE_E5_HZ
        )
        private val CALL_RINGTONE_INCOMING_DURATION_MS = longArrayOf(112L, 112L, 168L, 84L, 112L, 176L)
        private val CALL_RINGTONE_INCOMING_NOTE_GAIN = doubleArrayOf(1.00, 1.00, 1.00, 0.88, 0.92, 0.96)
        private val CALL_RINGTONE_OUTGOING_START_MS = longArrayOf(0L, 180L, 1560L, 1710L)
        private val CALL_RINGTONE_OUTGOING_FREQ_HZ = doubleArrayOf(
            CALL_RINGTONE_G5_HZ,
            CALL_RINGTONE_E5_HZ,
            CALL_RINGTONE_G5_HZ,
            CALL_RINGTONE_E5_HZ
        )
        private val CALL_RINGTONE_OUTGOING_DURATION_MS = longArrayOf(118L, 190L, 96L, 160L)
        private val CALL_RINGTONE_OUTGOING_NOTE_GAIN = doubleArrayOf(0.82, 0.88, 0.68, 0.72)
        private val CALL_RINGTONE_INCOMING_PARTIALS = doubleArrayOf(0.74, 0.18, 0.08)
        private val CALL_RINGTONE_OUTGOING_PARTIALS = doubleArrayOf(0.80, 0.15, 0.05)
        // Standard Bluetooth MAC-48 address format: 6 hex octets separated
        // by colons. Used to guard the BLE connect bridge methods before we
        // hand the string to BluetoothAdapter.getRemoteDevice, which throws
        // IllegalArgumentException on malformed input.
        private val BLE_MAC_RE = Regex("^([0-9A-Fa-f]{2}:){5}[0-9A-Fa-f]{2}$")
    }
    private var webViewRef: WebView? = null
    private var appBackCallback: OnBackPressedCallback? = null
    private val handler = Handler(Looper.getMainLooper())
    private var bleGatt: RatspeakBleGatt? = null
    private var pendingTop = 0
    private var pendingBottom = 0
    private var pendingNavigate: String? = null
    private var usbPermissionReceiver: BroadcastReceiver? = null
    private var networkCallback: ConnectivityManager.NetworkCallback? = null
    private var pendingIdentityExport: PendingIdentityExport? = null
    private var pendingGenericFileSave: PendingFileSave? = null
    private var pendingMediaRequestId: String? = null
    private var pendingMediaRequestAudio = false
    private var pendingMediaRequestCamera = false
    private var callRingtoneGeneration = 0
    private var callRingtoneMode: String? = null
    private var callRingtoneTrack: AudioTrack? = null
    private var callRingtoneFocusRequest: Any? = null
    private var callAudioFocusRequest: Any? = null
    private var callProximityWakeLock: PowerManager.WakeLock? = null
    private var callAudioRouteActive = false
    private var callAudioRouteName: String? = null
    private val callRingtoneFocusListener = AudioManager.OnAudioFocusChangeListener { change ->
        if (change == AudioManager.AUDIOFOCUS_LOSS || change == AudioManager.AUDIOFOCUS_LOSS_TRANSIENT) {
            handler.post { stopNativeCallRingtone() }
        }
    }
    private val callAudioFocusListener = AudioManager.OnAudioFocusChangeListener { }
    @Volatile private var lastNetworkType: String = ""
    @Volatile private var serviceMulticastEnabled = false

    private data class PendingIdentityExport(val fileName: String, val bytes: ByteArray)
    private data class PendingFileSave(
        val requestId: String,
        val fileName: String,
        val bytes: ByteArray,
        val mimeType: String
    )

    private val identityBackupDocumentLauncher: ActivityResultLauncher<Intent> =
        registerForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
            handleIdentityBackupDocumentResult(result.resultCode, result.data)
        }

    private val identityImportDocumentLauncher: ActivityResultLauncher<Intent> =
        registerForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
            handleIdentityImportDocumentResult(result.resultCode, result.data)
        }

    private val genericFileDocumentLauncher: ActivityResultLauncher<Intent> =
        registerForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
            handleGenericFileDocumentResult(result.resultCode, result.data)
        }

    override fun onWebViewCreate(webView: WebView) {
        super.onWebViewCreate(webView)
        // Local app assets are served through WebViewAssetLoader. Same-version
        // APK reinstalls during development can otherwise keep stale HTML/CSS.
        try {
            webView.clearCache(true)
        } catch (e: Exception) {
            Log.d("Ratspeak", "clearCache: ${e.javaClass.simpleName}: ${e.message}")
        }
        // Allow the loading page (served over https://tauri.localhost/) to fetch
        // the embedded HTTP backend at http://127.0.0.1:<port>. Without this,
        // Android WebView blocks the request as mixed content (default on API 21+).
        webView.settings.mixedContentMode = android.webkit.WebSettings.MIXED_CONTENT_ALWAYS_ALLOW
        // Incoming call ringtones are app audio, not microphone capture. Allow
        // Web Audio playback after startup notification permission handling.
        webView.settings.mediaPlaybackRequiresUserGesture = false
        webViewRef = webView
        installAppBackNavigation()
        // Expose BLE permission bridge to JavaScript
        webView.addJavascriptInterface(BlePermissionBridge(), "RatspeakAndroid")
        // Inject any insets that arrived before WebView was ready
        injectInsets()
        // Re-inject periodically to survive page navigation (loading -> dashboard)
        var count = 0
        handler.postDelayed(object : Runnable {
            override fun run() {
                if (count < 5) {
                    injectInsets()
                    count++
                    handler.postDelayed(this, 2000)
                }
            }
        }, 2000)
        // Start polling for theme changes from the WebView
        startThemePolling()
        // Handle pending navigation from notification tap
        pendingNavigate?.let { target ->
            pendingNavigate = null
            // Delay to let the page fully load
            handler.postDelayed({
                navigateToView(target)
            }, 3000)
        }
    }

    private fun installAppBackNavigation() {
        if (appBackCallback != null) return
        val callback = object : OnBackPressedCallback(true) {
            override fun handleOnBackPressed() {
                dispatchBackToWebView()
            }
        }
        appBackCallback = callback
        onBackPressedDispatcher.addCallback(this, callback)
    }

    private fun dispatchBackToWebView() {
        val webView = webViewRef
        if (webView == null) {
            continueSystemBack()
            return
        }

        webView.evaluateJavascript(
            """
            (function() {
              try {
                return !!(window.RS &&
                  typeof window.RS.handleAndroidBack === 'function' &&
                  window.RS.handleAndroidBack());
              } catch (e) {
                return false;
              }
            })();
            """.trimIndent()
        ) { rawResult ->
            if (rawResult == "true") return@evaluateJavascript
            continueSystemBack()
        }
    }

    private fun continueSystemBack() {
        val callback = appBackCallback
        callback?.isEnabled = false
        try {
            onBackPressedDispatcher.onBackPressed()
        } finally {
            callback?.isEnabled = true
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)

        // Check for notification navigation intent
        handleNavigateIntent(intent)

        // Match splash background to OS theme preference
        val isDarkMode = (resources.configuration.uiMode and
            android.content.res.Configuration.UI_MODE_NIGHT_MASK) ==
            android.content.res.Configuration.UI_MODE_NIGHT_YES
        val bgColor = if (isDarkMode) "#18171a" else "#FAF7F3"
        window.decorView.setBackgroundColor(android.graphics.Color.parseColor(bgColor))

        // Both bars transparent — WebView CSS renders the safe areas
        window.statusBarColor = android.graphics.Color.TRANSPARENT
        window.navigationBarColor = android.graphics.Color.TRANSPARENT
        window.isNavigationBarContrastEnforced = false

        // Set initial bar icon appearance based on OS theme
        WindowCompat.getInsetsController(window, window.decorView).apply {
            isAppearanceLightStatusBars = !isDarkMode
            isAppearanceLightNavigationBars = !isDarkMode
        }

        ViewCompat.setOnApplyWindowInsetsListener(findViewById(android.R.id.content)) { view, insets ->
            val bars = insets.getInsets(WindowInsetsCompat.Type.systemBars())
            val ime = insets.getInsets(WindowInsetsCompat.Type.ime())

            // No native top/bottom padding — CSS handles safe areas
            // Only IME keyboard pushes content up
            view.setPadding(bars.left, 0, bars.right, if (ime.bottom > 0) ime.bottom else 0)

            // Convert physical pixels to CSS pixels (dp)
            val density = view.resources.displayMetrics.density
            pendingTop = Math.round(bars.top / density)
            pendingBottom = Math.round(bars.bottom / density)
            injectInsets()

            insets
        }

        // Start foreground service
        val serviceIntent = Intent(this, RatspeakService::class.java)
        startForegroundService(serviceIntent)

        // Android 13+ gates notifications behind a runtime permission. Without
        // it, NotificationManager.notify() silently drops — including message
        // notifications emitted by the Rust/Tauri notification backend. Request it at
        // startup, once, so the prompt lands before the first inbound message.
        // BLE permissions are requested on-demand via the JS bridge and use a
        // different request code, so the two dialogs don't overlap.
        requestNotificationPermissionIfNeeded()

        registerNetworkCallback()
    }

    private fun requestNotificationPermissionIfNeeded() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) return
        val perm = Manifest.permission.POST_NOTIFICATIONS
        if (ContextCompat.checkSelfPermission(this, perm) == PackageManager.PERMISSION_GRANTED) return
        ActivityCompat.requestPermissions(this, arrayOf(perm), NOTIFICATION_PERMISSION_REQUEST_CODE)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handleNavigateIntent(intent)
    }

    override fun onResume() {
        super.onResume()
        postLifecycleState(true)
        // ACTION_REFRESH clears per-sender notifications in RatspeakService
        // and kicks the poll loop so lastKnownUnread is current before the
        // user reads messages to zero.
        refreshServicePoll()
    }

    override fun onPause() {
        super.onPause()
        // Signal background state to Rust backend (fallback for JS visibilitychange)
        postLifecycleState(false)
        refreshServicePoll()
    }

    private fun refreshServicePoll() {
        try {
            val intent = Intent(this, RatspeakService::class.java).apply {
                action = RatspeakService.ACTION_REFRESH
            }
            startService(intent)
        } catch (_: Exception) {
            // Service not running yet (first onCreate hasn't finished) — safe
            // to skip; the service will do its first poll as soon as it's up.
        }
    }

    override fun onDestroy() {
        // The foreground service (RatspeakService) owns mesh lifetime, but the BLE
        // GATT handle lives on this Activity. If the Activity is destroyed, close
        // the GATT link cleanly so we don't leak a stale BluetoothGatt into the
        // OS stack.
        try { bleGatt?.disconnect() } catch (_: Exception) {}
        bleGatt = null
        usbPermissionReceiver?.let {
            try { unregisterReceiver(it) } catch (_: Exception) {}
        }
        usbPermissionReceiver = null
        networkCallback?.let {
            try {
                getSystemService(ConnectivityManager::class.java).unregisterNetworkCallback(it)
            } catch (_: Exception) {}
        }
        networkCallback = null
        releaseCallProximityWakeLock(waitForNoProximity = false)
        super.onDestroy()
    }

    /**
     * Register a default-network callback so the Rust core re-evaluates Auto
     * transport mode whenever the OS reports a network change (wifi↔cellular
     * handoff, gain/loss). We invoke `network_type_changed` via the Tauri
     * IPC bridge — ConnectivityManager fires on the actual network transition
     * rather than the WebView's lagging navigator.connection proxy.
     *
     * The iOS side mirrors this with NWPathMonitor in src-tauri/src/lib.rs.
     */
    private fun registerNetworkCallback() {
        if (networkCallback != null) return
        val cm = try {
            getSystemService(ConnectivityManager::class.java)
        } catch (_: Exception) { null } ?: return

        val cb = object : ConnectivityManager.NetworkCallback() {
            override fun onAvailable(network: Network) {
                emitIfChanged(cm.getNetworkCapabilities(network))
            }

            override fun onCapabilitiesChanged(network: Network, caps: NetworkCapabilities) {
                emitIfChanged(caps)
            }

            override fun onLost(network: Network) {
                emitIfChanged(null)
            }

            private fun emitIfChanged(caps: NetworkCapabilities?) {
                val type = classifyTransport(caps)
                if (type == lastNetworkType) return
                lastNetworkType = type
                updateServiceMulticastLock(type == "wifi")
                injectNetworkTypeChange(type)
            }
        }
        try {
            cm.registerDefaultNetworkCallback(cb)
            networkCallback = cb
        } catch (_: Exception) {}
    }

    private fun classifyTransport(caps: NetworkCapabilities?): String {
        if (caps == null) return "none"
        return when {
            caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) -> "wifi"
            caps.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR) -> "cellular"
            caps.hasTransport(NetworkCapabilities.TRANSPORT_ETHERNET) -> "ethernet"
            else -> "unknown"
        }
    }

    private fun updateServiceMulticastLock(enable: Boolean) {
        if (serviceMulticastEnabled == enable) return
        serviceMulticastEnabled = enable
        try {
            val intent = Intent(this, RatspeakService::class.java).apply {
                action = if (enable) {
                    RatspeakService.ACTION_ENABLE_MULTICAST
                } else {
                    RatspeakService.ACTION_DISABLE_MULTICAST
                }
            }
            startService(intent)
        } catch (_: Exception) {}
    }

    /**
     * Route the path-change through Tauri IPC so the Rust core's
     * `network_type_changed` command can re-evaluate Auto transport mode.
     * `typeof RS !== 'undefined'` guards the early-boot window before
     * state.js has defined the IPC wrapper.
     */
    private fun injectNetworkTypeChange(networkType: String) {
        webViewRef?.post {
            webViewRef?.evaluateJavascript(
                "if (typeof RS !== 'undefined' && RS.invoke) { " +
                    "RS.invoke('network_type_changed', { args: { network_type: '$networkType' } }).catch(function(){}); }",
                null
            )
        }
    }

    /** Route the foreground/background transition through Tauri IPC — the core's
     *  `api_set_foreground` command handles everything else. WebView JS is the
     *  one-line bridge from native Activity callbacks to the Tauri runtime.
     */
    private fun postLifecycleState(foreground: Boolean) {
        webViewRef?.post {
            webViewRef?.evaluateJavascript(
                "if (typeof RS !== 'undefined' && RS.invoke) { " +
                    "RS.invoke('api_set_foreground', { args: { foreground: $foreground } }).catch(function(){}); }",
                null
            )
        }
    }

    private fun handleNavigateIntent(intent: Intent?) {
        val target = intent?.getStringExtra("navigate_to") ?: return
        val destHash = intent.getStringExtra("dest_hash")
        val payload = if (!destHash.isNullOrEmpty()) "$target|$destHash" else target
        if (webViewRef != null) {
            navigateToView(payload)
        } else {
            pendingNavigate = payload
        }
    }

    private fun navigateToView(payload: String) {
        val parts = payload.split("|", limit = 2)
        val view = parts[0]
        val destHash = parts.getOrNull(1) ?: ""
        // Encode each argument as a JSON string literal so a stray quote (or
        // any character that would escape the surrounding JS string) can't
        // break the injection. `JSONObject.quote` returns a double-quoted
        // JSON string including the surrounding `"`, which is a valid JS
        // expression on its own.
        val viewJs = org.json.JSONObject.quote(view)
        val js = buildString {
            append("if(typeof switchView==='function')switchView(").append(viewJs).append(");")
            if (destHash.isNotEmpty()) {
                val destJs = org.json.JSONObject.quote(destHash)
                append("setTimeout(function(){if(typeof openConversationWith==='function')openConversationWith(")
                append(destJs)
                append(");},150);")
            }
        }
        webViewRef?.evaluateJavascript(js, null)
    }

    private fun injectInsets() {
        webViewRef?.evaluateJavascript(
            "document.documentElement.style.setProperty('--sat','${pendingTop}px');" +
            "document.documentElement.style.setProperty('--sab','${pendingBottom}px');",
            null
        )
    }

    /**
     * Poll the WebView's data-theme attribute and update system bar icon colors.
     * Runs every 3s for 30s after page load to catch theme changes during init,
     * then stops. User-initiated theme changes after that are cosmetic-only for
     * the status bar until next app restart.
     */
    private fun startThemePolling() {
        var pollCount = 0
        val maxPolls = 10
        handler.postDelayed(object : Runnable {
            override fun run() {
                if (pollCount >= maxPolls) return
                pollCount++
                webViewRef?.evaluateJavascript(
                    "(function(){return document.documentElement.getAttribute('data-theme')||''})()"
                ) { value ->
                    // evaluateJavascript returns JSON-quoted string e.g. "\"dark\""
                    val theme = value?.trim()?.removeSurrounding("\"") ?: ""
                    if (theme == "light" || theme == "dark") {
                        val isLight = theme == "light"
                        handler.post {
                            WindowCompat.getInsetsController(window, window.decorView).apply {
                                isAppearanceLightStatusBars = isLight
                                isAppearanceLightNavigationBars = isLight
                            }
                        }
                    }
                }
                handler.postDelayed(this, 3000)
            }
        }, 3000)
    }

    // ---- BLE permission helpers ----

    private fun getBlePermissions(): Array<String> {
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

    private fun hasBlePermissions(): Boolean {
        return getBlePermissions().all {
            ContextCompat.checkSelfPermission(this, it) == PackageManager.PERMISSION_GRANTED
        }
    }

    private fun getMediaPermissions(audio: Boolean, camera: Boolean): Array<String> {
        val permissions = mutableListOf<String>()
        if (camera) permissions.add(Manifest.permission.CAMERA)
        if (audio) permissions.add(Manifest.permission.RECORD_AUDIO)
        return permissions.toTypedArray()
    }

    private fun hasMediaPermissions(audio: Boolean, camera: Boolean): Boolean {
        val permissions = getMediaPermissions(audio, camera)
        if (permissions.isEmpty()) return true
        return permissions.all {
            ContextCompat.checkSelfPermission(this, it) == PackageManager.PERMISSION_GRANTED
        }
    }

    private fun isKnownGoogleWebViewPackage(packageName: String): Boolean {
        return when (packageName.lowercase()) {
            "com.google.android.webview",
            "com.android.chrome",
            "com.chrome.beta",
            "com.chrome.dev",
            "com.chrome.canary" -> true
            else -> false
        }
    }

    private fun packageLabel(packageName: String): String {
        return try {
            val appInfo = packageManager.getApplicationInfo(packageName, 0)
            packageManager.getApplicationLabel(appInfo).toString()
        } catch (_: Throwable) {
            ""
        }
    }

    private fun buildQrScannerEnvironment(): String {
        val webViewPackageInfo = try {
            WebViewCompat.getCurrentWebViewPackage(this)
        } catch (_: Throwable) {
            null
        }
        val webViewPackage = webViewPackageInfo?.packageName ?: ""
        val gmsLabel = packageLabel("com.google.android.gms")
        val microGDetected = gmsLabel.contains("microg", ignoreCase = true)
        val preferLive = isKnownGoogleWebViewPackage(webViewPackage) && !microGDetected
        val reason = when {
            microGDetected -> "microg"
            webViewPackage.isBlank() -> "unknown_webview"
            preferLive -> "google_webview"
            else -> "non_google_webview"
        }
        return JSONObject().apply {
            put("platform", "android")
            put("webview_package", webViewPackage)
            put("webview_version", webViewPackageInfo?.versionName ?: "")
            put("microg_detected", microGDetected)
            put("prefer_live_scanner", preferLive)
            put("reason", reason)
        }.toString()
    }

    private fun normalizedCallRingtoneMode(mode: String): String {
        return if (mode.equals("incoming", ignoreCase = true)) "incoming" else "outgoing"
    }

    private fun startNativeCallRingtone(mode: String): Boolean {
        val normalizedMode = normalizedCallRingtoneMode(mode)
        stopNativeCallRingtone()
        callRingtoneMode = normalizedMode
        callRingtoneGeneration++
        val generation = callRingtoneGeneration
        volumeControlStream = if (normalizedMode == "incoming") {
            AudioManager.STREAM_RING
        } else {
            AudioManager.STREAM_VOICE_CALL
        }
        configureCallRingtoneRoute(normalizedMode)
        if (!requestCallRingtoneAudioFocus(normalizedMode)) {
            if (callRingtoneGeneration == generation) stopNativeCallRingtone()
            return false
        }
        val started = playNativeCallRingtoneLoop(normalizedMode, generation)
        if (!started && callRingtoneGeneration == generation) stopNativeCallRingtone()
        return started
    }

    private fun stopNativeCallRingtone() {
        callRingtoneGeneration++
        callRingtoneMode = null
        callRingtoneTrack?.let { track ->
            try { track.stop() } catch (_: Throwable) {}
            try { track.release() } catch (_: Throwable) {}
        }
        callRingtoneTrack = null
        abandonCallRingtoneAudioFocus()
        if (!callAudioRouteActive) {
            volumeControlStream = AudioManager.USE_DEFAULT_STREAM_TYPE
            restoreCallAudioRoute()
        }
    }

    private fun requestCallRingtoneAudioFocus(mode: String): Boolean {
        val audioManager = getSystemService(Context.AUDIO_SERVICE) as? AudioManager ?: return false
        val attributes = callRingtoneAudioAttributes(mode)
        val result = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val request = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN_TRANSIENT)
                .setAudioAttributes(attributes)
                .setOnAudioFocusChangeListener(callRingtoneFocusListener, handler)
                .build()
            val focusResult = audioManager.requestAudioFocus(request)
            if (focusResult == AudioManager.AUDIOFOCUS_REQUEST_GRANTED) {
                callRingtoneFocusRequest = request
            }
            focusResult
        } else {
            @Suppress("DEPRECATION")
            audioManager.requestAudioFocus(
                callRingtoneFocusListener,
                if (mode == "incoming") AudioManager.STREAM_RING else AudioManager.STREAM_VOICE_CALL,
                AudioManager.AUDIOFOCUS_GAIN_TRANSIENT
            )
        }
        return result == AudioManager.AUDIOFOCUS_REQUEST_GRANTED
    }

    private fun abandonCallRingtoneAudioFocus() {
        val audioManager = getSystemService(Context.AUDIO_SERVICE) as? AudioManager ?: return
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val request = callRingtoneFocusRequest as? AudioFocusRequest
            if (request != null) {
                audioManager.abandonAudioFocusRequest(request)
                callRingtoneFocusRequest = null
                return
            }
        }
        run {
            @Suppress("DEPRECATION")
            audioManager.abandonAudioFocus(callRingtoneFocusListener)
        }
    }

    private fun requestCallAudioFocus() {
        val audioManager = getSystemService(Context.AUDIO_SERVICE) as? AudioManager ?: return
        val attributes = AudioAttributes.Builder()
            .setUsage(AudioAttributes.USAGE_VOICE_COMMUNICATION)
            .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
            .build()
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val existing = callAudioFocusRequest as? AudioFocusRequest
            if (existing != null) return
            val request = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN_TRANSIENT)
                .setAudioAttributes(attributes)
                .setOnAudioFocusChangeListener(callAudioFocusListener, handler)
                .build()
            callAudioFocusRequest = request
            audioManager.requestAudioFocus(request)
        } else {
            @Suppress("DEPRECATION")
            audioManager.requestAudioFocus(
                callAudioFocusListener,
                AudioManager.STREAM_VOICE_CALL,
                AudioManager.AUDIOFOCUS_GAIN_TRANSIENT
            )
        }
    }

    private fun abandonCallAudioFocus() {
        val audioManager = getSystemService(Context.AUDIO_SERVICE) as? AudioManager ?: return
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val request = callAudioFocusRequest as? AudioFocusRequest
            if (request != null) {
                audioManager.abandonAudioFocusRequest(request)
                callAudioFocusRequest = null
                return
            }
        }
        @Suppress("DEPRECATION")
        audioManager.abandonAudioFocus(callAudioFocusListener)
    }

    private fun callRingtoneAudioAttributes(mode: String): AudioAttributes {
        val usage = if (mode == "incoming") {
            AudioAttributes.USAGE_NOTIFICATION_RINGTONE
        } else {
            AudioAttributes.USAGE_VOICE_COMMUNICATION_SIGNALLING
        }
        return AudioAttributes.Builder()
            .setUsage(usage)
            .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
            .build()
    }

    private fun configureCallRingtoneRoute(mode: String) {
        val audioManager = getSystemService(Context.AUDIO_SERVICE) as? AudioManager ?: return
        if (mode == "incoming") {
            if (!callAudioRouteActive) {
                restoreCallAudioRoute()
                audioManager.mode = AudioManager.MODE_RINGTONE
            }
            return
        }
        configureCommunicationRoute(preferEarpiece = true)
    }

    private fun startNativeCallAudioRoute(role: String) {
        val routeName = if (role.equals("speaker", ignoreCase = true)) "speaker" else "earpiece"
        callAudioRouteActive = true
        callAudioRouteName = routeName
        volumeControlStream = AudioManager.STREAM_VOICE_CALL
        requestCallAudioFocus()
        val preferEarpiece = routeName != "speaker"
        configureCommunicationRoute(preferEarpiece)
        syncCallProximityWakeLock(preferEarpiece)
    }

    private fun stopNativeCallAudioRoute() {
        callAudioRouteActive = false
        callAudioRouteName = null
        releaseCallProximityWakeLock()
        restoreCallAudioRoute()
        volumeControlStream = AudioManager.USE_DEFAULT_STREAM_TYPE
        abandonCallAudioFocus()
    }

    private fun configureCommunicationRoute(preferEarpiece: Boolean) {
        val audioManager = getSystemService(Context.AUDIO_SERVICE) as? AudioManager ?: return
        audioManager.mode = AudioManager.MODE_IN_COMMUNICATION
        @Suppress("DEPRECATION")
        audioManager.isSpeakerphoneOn = !preferEarpiece
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            val route = selectCommunicationDevice(audioManager, preferEarpiece)
            if (route != null) {
                try {
                    val current = audioManager.communicationDevice
                    if (current != null && current.type != route.type) {
                        audioManager.clearCommunicationDevice()
                    }
                    if (!audioManager.setCommunicationDevice(route)) {
                        @Suppress("DEPRECATION")
                        audioManager.isSpeakerphoneOn = !preferEarpiece
                    }
                } catch (_: Throwable) {
                    @Suppress("DEPRECATION")
                    audioManager.isSpeakerphoneOn = !preferEarpiece
                }
            } else {
                try { audioManager.clearCommunicationDevice() } catch (_: Throwable) {}
            }
        } else {
            @Suppress("DEPRECATION")
            audioManager.isSpeakerphoneOn = !preferEarpiece
        }
    }

    private fun restoreCallAudioRoute() {
        releaseCallProximityWakeLock()
        val audioManager = getSystemService(Context.AUDIO_SERVICE) as? AudioManager ?: return
        @Suppress("DEPRECATION")
        audioManager.isSpeakerphoneOn = false
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            try { audioManager.clearCommunicationDevice() } catch (_: Throwable) {}
        } else {
            @Suppress("DEPRECATION")
            audioManager.isSpeakerphoneOn = false
        }
        audioManager.mode = AudioManager.MODE_NORMAL
    }

    private fun selectCommunicationDevice(
        audioManager: AudioManager,
        preferEarpiece: Boolean
    ): AudioDeviceInfo? {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.S) return null
        val devices = try {
            audioManager.availableCommunicationDevices
        } catch (_: Throwable) {
            return null
        }
        if (!preferEarpiece) {
            val speaker = devices.firstOrNull {
                it.isSink && it.type == AudioDeviceInfo.TYPE_BUILTIN_SPEAKER
            }
            if (speaker != null) return speaker
        }
        val accessory = devices.firstOrNull { device ->
            device.isSink && when (device.type) {
                AudioDeviceInfo.TYPE_BLUETOOTH_SCO,
                AudioDeviceInfo.TYPE_BLE_HEADSET,
                AudioDeviceInfo.TYPE_USB_HEADSET,
                AudioDeviceInfo.TYPE_WIRED_HEADSET,
                AudioDeviceInfo.TYPE_WIRED_HEADPHONES -> true
                else -> false
            }
        }
        if (accessory != null) return accessory
        val preferredType = if (preferEarpiece) {
            AudioDeviceInfo.TYPE_BUILTIN_EARPIECE
        } else {
            AudioDeviceInfo.TYPE_BUILTIN_SPEAKER
        }
        return devices.firstOrNull { it.isSink && it.type == preferredType }
            ?: devices.firstOrNull { it.isSink && it.type == AudioDeviceInfo.TYPE_BUILTIN_SPEAKER }
    }

    private fun syncCallProximityWakeLock(preferEarpiece: Boolean) {
        if (preferEarpiece) {
            acquireCallProximityWakeLock()
        } else {
            releaseCallProximityWakeLock()
        }
    }

    private fun acquireCallProximityWakeLock() {
        val powerManager = getSystemService(Context.POWER_SERVICE) as? PowerManager ?: return
        if (!powerManager.isWakeLockLevelSupported(PowerManager.PROXIMITY_SCREEN_OFF_WAKE_LOCK)) return
        val lock = callProximityWakeLock ?: try {
            powerManager
                .newWakeLock(
                    PowerManager.PROXIMITY_SCREEN_OFF_WAKE_LOCK,
                    "Ratspeak:LXSTProximity"
                )
                .apply { setReferenceCounted(false) }
                .also { callProximityWakeLock = it }
        } catch (_: Throwable) {
            null
        } ?: return
        if (!lock.isHeld) {
            try { lock.acquire() } catch (_: Throwable) {}
        }
    }

    private fun releaseCallProximityWakeLock(waitForNoProximity: Boolean = true) {
        val lock = callProximityWakeLock ?: return
        try {
            if (lock.isHeld) {
                if (waitForNoProximity) {
                    lock.release(PowerManager.RELEASE_FLAG_WAIT_FOR_NO_PROXIMITY)
                } else {
                    lock.release()
                }
            }
        } catch (_: Throwable) {}
        callProximityWakeLock = null
    }

    private fun callRingtoneSequenceMs(): Long {
        return CALL_RINGTONE_LOOP_MS
    }

    private fun callRingtoneNoteCount(mode: String): Int {
        return if (mode == "incoming") {
            CALL_RINGTONE_INCOMING_FREQ_HZ.size
        } else {
            CALL_RINGTONE_OUTGOING_FREQ_HZ.size
        }
    }

    private fun callRingtoneNoteStartMs(mode: String, noteIndex: Int): Long {
        return if (mode == "incoming") {
            CALL_RINGTONE_INCOMING_START_MS[noteIndex]
        } else {
            CALL_RINGTONE_OUTGOING_START_MS[noteIndex]
        }
    }

    private fun callRingtoneNoteFrequency(mode: String, noteIndex: Int): Double {
        return if (mode == "incoming") {
            CALL_RINGTONE_INCOMING_FREQ_HZ[noteIndex]
        } else {
            CALL_RINGTONE_OUTGOING_FREQ_HZ[noteIndex]
        }
    }

    private fun callRingtoneNoteDurationMs(mode: String, noteIndex: Int): Long {
        return if (mode == "incoming") {
            CALL_RINGTONE_INCOMING_DURATION_MS[noteIndex]
        } else {
            CALL_RINGTONE_OUTGOING_DURATION_MS[noteIndex]
        }
    }

    private fun callRingtoneNoteGain(mode: String, noteIndex: Int): Double {
        return if (mode == "incoming") {
            CALL_RINGTONE_INCOMING_NOTE_GAIN[noteIndex]
        } else {
            CALL_RINGTONE_OUTGOING_NOTE_GAIN[noteIndex]
        }
    }

    private fun callRingtonePartials(mode: String): DoubleArray {
        return if (mode == "incoming") {
            CALL_RINGTONE_INCOMING_PARTIALS
        } else {
            CALL_RINGTONE_OUTGOING_PARTIALS
        }
    }

    private fun callRingtoneVolume(mode: String): Double {
        return if (mode == "incoming") {
            CALL_RINGTONE_INCOMING_VOLUME
        } else {
            CALL_RINGTONE_OUTGOING_VOLUME
        }
    }

    private fun callRingtoneGlideCents(mode: String): Double {
        return if (mode == "incoming") {
            CALL_RINGTONE_INCOMING_GLIDE_CENTS
        } else {
            CALL_RINGTONE_OUTGOING_GLIDE_CENTS
        }
    }

    private fun callRingtoneAttackMs(mode: String): Long {
        return if (mode == "incoming") {
            CALL_RINGTONE_INCOMING_ATTACK_MS
        } else {
            CALL_RINGTONE_OUTGOING_ATTACK_MS
        }
    }

    private fun callRingtoneReleaseMs(mode: String): Long {
        return if (mode == "incoming") {
            CALL_RINGTONE_INCOMING_RELEASE_MS
        } else {
            CALL_RINGTONE_OUTGOING_RELEASE_MS
        }
    }

    private fun playNativeCallRingtoneLoop(mode: String, generation: Int): Boolean {
        val pcm = buildNativeCallRingtonePcm(mode)
        val frameCount = pcm.size / 2
        if (callRingtoneGeneration != generation) return false
        val track = try {
            AudioTrack.Builder()
                .setAudioAttributes(callRingtoneAudioAttributes(mode))
                .setAudioFormat(
                    AudioFormat.Builder()
                        .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                        .setSampleRate(CALL_RINGTONE_SAMPLE_RATE)
                        .setChannelMask(AudioFormat.CHANNEL_OUT_MONO)
                        .build()
                )
                .setTransferMode(AudioTrack.MODE_STATIC)
                .setBufferSizeInBytes(pcm.size)
                .build()
        } catch (_: Throwable) {
            return false
        }
        try {
            val written = track.write(pcm, 0, pcm.size)
            if (written != pcm.size) {
                try { track.release() } catch (_: Throwable) {}
                return false
            }
            track.setLoopPoints(0, frameCount, -1)
            callRingtoneTrack = track
            track.play()
            return track.playState == AudioTrack.PLAYSTATE_PLAYING
        } catch (_: Throwable) {
            if (callRingtoneTrack === track) callRingtoneTrack = null
            try { track.release() } catch (_: Throwable) {}
            return false
        }
    }

    private fun buildNativeCallRingtonePcm(mode: String): ByteArray {
        val volume = callRingtoneVolume(mode)
        val partials = callRingtonePartials(mode)
        val totalSamples = ((CALL_RINGTONE_SAMPLE_RATE * callRingtoneSequenceMs()) / 1000L)
            .toInt()
            .coerceAtLeast(1)
        val samples = DoubleArray(totalSamples)
        for (noteIndex in 0 until callRingtoneNoteCount(mode)) {
            mixNativeCallTone(
                samples,
                callRingtoneNoteStartMs(mode, noteIndex),
                callRingtoneNoteFrequency(mode, noteIndex),
                callRingtoneNoteDurationMs(mode, noteIndex),
                volume,
                callRingtoneNoteGain(mode, noteIndex),
                callRingtoneGlideCents(mode),
                callRingtoneAttackMs(mode),
                callRingtoneReleaseMs(mode),
                partials
            )
        }
        return samplesToPcm16(samples)
    }

    private fun raisedCosine(progress: Double): Double {
        val x = progress.coerceIn(0.0, 1.0)
        return 0.5 - (0.5 * cos(PI * x))
    }

    private fun mixNativeCallTone(
        output: DoubleArray,
        startMs: Long,
        freq: Double,
        durationMs: Long,
        volume: Double,
        noteGain: Double,
        glideCents: Double,
        attackMs: Long,
        releaseMs: Long,
        partials: DoubleArray
    ) {
        val sampleCount = ((CALL_RINGTONE_SAMPLE_RATE * durationMs) / 1000L).toInt()
        val startSample = ((CALL_RINGTONE_SAMPLE_RATE * startMs) / 1000L).toInt()
        val attackDurationMs = attackMs.toDouble().coerceAtLeast(1.0)
        val releaseDurationMs = releaseMs.toDouble().coerceAtLeast(1.0)
        val secondPartialPhase = 0.35 * PI
        val airPartialPhase = 0.10 * PI
        var phase = 0.0
        for (i in 0 until sampleCount) {
            val outputIndex = startSample + i
            if (outputIndex !in output.indices) break
            val progress = if (sampleCount > 1) i.toDouble() / (sampleCount - 1).toDouble() else 0.0
            val elapsedMs = (i.toDouble() * 1000.0) / CALL_RINGTONE_SAMPLE_RATE.toDouble()
            val remainingMs = ((sampleCount - i - 1).toDouble() * 1000.0) /
                CALL_RINGTONE_SAMPLE_RATE.toDouble()
            val instantFreq = freq * 2.0.pow((glideCents * progress) / 1200.0)
            phase += (2.0 * PI * instantFreq) / CALL_RINGTONE_SAMPLE_RATE.toDouble()
            var envelope = raisedCosine(elapsedMs / attackDurationMs)
            if (remainingMs < releaseDurationMs) {
                envelope *= raisedCosine(remainingMs / releaseDurationMs)
            }
            val tone = (partials[0] * sin(phase)) +
                (partials[1] * sin((phase * 2.0) + secondPartialPhase)) +
                (partials[2] * sin((phase * 1.5) + airPartialPhase))
            val sample = (tone * envelope * volume * noteGain)
                .coerceIn(-1.0, 1.0)
            output[outputIndex] = (output[outputIndex] + sample).coerceIn(-1.0, 1.0)
        }
    }

    private fun samplesToPcm16(samples: DoubleArray): ByteArray {
        val bytes = ByteArray(samples.size * 2)
        for (i in samples.indices) {
            val shortValue = (samples[i].coerceIn(-1.0, 1.0) * Short.MAX_VALUE).toInt().toShort()
            val offset = i * 2
            bytes[offset] = (shortValue.toInt() and 0xff).toByte()
            bytes[offset + 1] = ((shortValue.toInt() shr 8) and 0xff).toByte()
        }
        return bytes
    }

    private fun runOnMainForBoolean(timeoutMs: Long = 500L, block: () -> Boolean): Boolean {
        if (Looper.myLooper() == Looper.getMainLooper()) {
            return try { block() } catch (_: Throwable) { false }
        }
        val latch = CountDownLatch(1)
        var result = false
        handler.post {
            result = try { block() } catch (_: Throwable) { false }
            latch.countDown()
        }
        return try {
            latch.await(timeoutMs, TimeUnit.MILLISECONDS) && result
        } catch (_: InterruptedException) {
            Thread.currentThread().interrupt()
            false
        }
    }

    override fun onRequestPermissionsResult(
        requestCode: Int,
        permissions: Array<out String>,
        grantResults: IntArray
    ) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        if (requestCode == BLE_PERMISSION_REQUEST_CODE) {
            val granted = grantResults.isNotEmpty() && grantResults.all {
                it == PackageManager.PERMISSION_GRANTED
            }
            // Notify the WebView of the permission result
            handler.post {
                webViewRef?.evaluateJavascript(
                    "if(typeof window._onBlePermissionResult==='function')window._onBlePermissionResult($granted);",
                    null
                )
            }
        } else if (requestCode == NOTIFICATION_PERMISSION_REQUEST_CODE) {
            // No action required — RatspeakService polls notifyManager directly
            // and will silently fail until the user re-grants via system
            // settings. Future work: expose a settings toggle and re-prompt
            // through shouldShowRequestPermissionRationale().
        } else if (requestCode == MEDIA_PERMISSION_REQUEST_CODE) {
            val requestId = pendingMediaRequestId ?: ""
            val audio = pendingMediaRequestAudio
            val camera = pendingMediaRequestCamera
            pendingMediaRequestId = null
            pendingMediaRequestAudio = false
            pendingMediaRequestCamera = false
            val granted = grantResults.isNotEmpty() && grantResults.all {
                it == PackageManager.PERMISSION_GRANTED
            }
            dispatchMediaPermissionResult(requestId, audio, camera, granted, null)
        }
    }

    // ---- Native BLE scanner (modern BluetoothManager API) ----

    private var bleScanner: BluetoothLeScanner? = null
    private var bleScanCallback: ScanCallback? = null

    // Nordic UART Service UUID — shared with RatspeakBleGatt + Rust side (see BleUuids.kt).
    private val NUS_SERVICE_UUID = BleUuids.NUS_SERVICE_PARCEL

    // Bluetooth Peer service UUIDs (Ratspeak primary + Columba compat). Used as
    // scan filters when JS calls scanForBlePeers(). Kept here, not in BleUuids,
    // because the static UUID strings already live in RatspeakBlePeerClient.
    private val RATSPEAK_PEER_SERVICE_UUID =
        ParcelUuid(RatspeakBlePeerClient.RATSPEAK_SERVICE)
    private val COLUMBA_PEER_SERVICE_UUID =
        ParcelUuid(RatspeakBlePeerClient.COLUMBA_SERVICE)

    @SuppressLint("MissingPermission")
    private fun startNativeBleScan(timeoutMs: Long = 5000) {
        val bluetoothManager = getSystemService(BluetoothManager::class.java)
        if (bluetoothManager == null) {
            sendBleScanResult(error = "Bluetooth service not available on this device")
            return
        }

        val adapter = bluetoothManager.adapter
        if (adapter == null) {
            sendBleScanResult(error = "No Bluetooth adapter found")
            return
        }

        if (!adapter.isEnabled) {
            sendBleScanResult(error = "Bluetooth is turned off. Enable it in system settings.")
            return
        }

        val scanner = adapter.bluetoothLeScanner
        if (scanner == null) {
            sendBleScanResult(error = "Bluetooth scanner unavailable. Try toggling Bluetooth off and on.")
            return
        }

        bleScanner = scanner
        val foundDevices = mutableMapOf<String, ScanResult>() // keyed by address to deduplicate

        val callback = object : ScanCallback() {
            override fun onScanResult(callbackType: Int, result: ScanResult) {
                val address = result.device.address ?: return
                // Keep the result with the strongest RSSI
                val existing = foundDevices[address]
                if (existing == null || (result.rssi > existing.rssi)) {
                    foundDevices[address] = result
                }
            }

            override fun onScanFailed(errorCode: Int) {
                val msg = when (errorCode) {
                    SCAN_FAILED_ALREADY_STARTED -> "Scan already in progress"
                    SCAN_FAILED_APPLICATION_REGISTRATION_FAILED -> "BLE app registration failed"
                    SCAN_FAILED_FEATURE_UNSUPPORTED -> "BLE scan not supported on this device"
                    SCAN_FAILED_INTERNAL_ERROR -> "Internal BLE error"
                    else -> "Scan failed (error $errorCode)"
                }
                sendBleScanResult(error = msg)
            }
        }

        bleScanCallback = callback

        val settings = ScanSettings.Builder()
            .setScanMode(ScanSettings.SCAN_MODE_LOW_LATENCY)
            .setCallbackType(ScanSettings.CALLBACK_TYPE_ALL_MATCHES)
            .build()

        scanner.startScan(null, settings, callback)

        // Stop after timeout and report results
        handler.postDelayed({
            try { scanner.stopScan(callback) } catch (_: Exception) {}
            bleScanCallback = null
            bleScanner = null

            val devices = JSONArray()
            for ((address, result) in foundDevices) {
                val name = result.device.name ?: result.scanRecord?.deviceName ?: ""
                if (name.isEmpty()) continue // Skip unnamed devices

                val serviceUuids = result.scanRecord?.serviceUuids ?: emptyList()
                // Require NUS service UUID *and* "RNode" name prefix so generic
                // Nordic-UART devices (Bangle.js, Adafruit demos, hobby boards)
                // don't pollute the picker. Name fallback still covers scan-response
                // quirks where service UUIDs are missing from the initial advert.
                val hasNus = serviceUuids.contains(NUS_SERVICE_UUID)
                val nameMatch = name.startsWith("RNode")
                val isRnode = (hasNus && nameMatch) || (serviceUuids.isEmpty() && nameMatch)
                if (!isRnode) continue

                val device = JSONObject().apply {
                    put("name", name)
                    put("address", address)
                    put("rssi", result.rssi)
                    put("device_type", "rnode")
                    put("bonded", result.device.bondState == BluetoothDevice.BOND_BONDED)
                }
                devices.put(device)
            }

            sendBleScanResult(devices = devices)
        }, timeoutMs)
    }

    private fun sendBleScanResult(devices: JSONArray? = null, error: String? = null) {
        val json = JSONObject().apply {
            put("devices", devices ?: JSONArray())
            if (error != null) put("error", error)
        }
        handler.post {
            webViewRef?.evaluateJavascript(
                "if(typeof window._onNativeBleScanResult==='function')window._onNativeBleScanResult(${json});",
                null
            )
        }
    }

    /**
     * Scan for Bluetooth Peer devices advertising the Ratspeak or Columba
     * service UUID. Distinct from [startNativeBleScan] which targets RNode
     * (NUS service); the two scans never overlap because they filter on
     * disjoint service UUIDs. Results delivered via window._onBlePeerScanResult.
     */
    @SuppressLint("MissingPermission")
    private fun startPeerBleScan(timeoutMs: Long = 5000) {
        val bm = getSystemService(BluetoothManager::class.java)
        if (bm == null) { sendPeerScanResult(error = "Bluetooth service unavailable"); return }
        val adapter = bm.adapter
        if (adapter == null) { sendPeerScanResult(error = "No Bluetooth adapter"); return }
        if (!adapter.isEnabled) { sendPeerScanResult(error = "Bluetooth is turned off"); return }
        val scanner = adapter.bluetoothLeScanner
        if (scanner == null) { sendPeerScanResult(error = "Bluetooth scanner unavailable"); return }

        val foundDevices = mutableMapOf<String, Pair<ScanResult, String>>() // address -> (result, protocol)

        val callback = object : ScanCallback() {
            override fun onScanResult(callbackType: Int, result: ScanResult) {
                val address = result.device.address ?: return
                val uuids = result.scanRecord?.serviceUuids ?: emptyList()
                val proto = when {
                    uuids.contains(RATSPEAK_PEER_SERVICE_UUID) -> "ratspeak"
                    uuids.contains(COLUMBA_PEER_SERVICE_UUID) -> "columba"
                    else -> return
                }
                val existing = foundDevices[address]
                // Prefer Ratspeak over Columba if both are advertised by one device.
                if (existing == null || result.rssi > existing.first.rssi ||
                    (existing.second == "columba" && proto == "ratspeak")) {
                    foundDevices[address] = result to proto
                }
            }
            override fun onScanFailed(errorCode: Int) {
                sendPeerScanResult(error = "Peer scan failed (error $errorCode)")
            }
        }

        // Filter on both service UUIDs so the OS does the work in firmware
        // — much friendlier on battery than scanning blind and filtering
        // in software.
        val filters = listOf(
            android.bluetooth.le.ScanFilter.Builder().setServiceUuid(RATSPEAK_PEER_SERVICE_UUID).build(),
            android.bluetooth.le.ScanFilter.Builder().setServiceUuid(COLUMBA_PEER_SERVICE_UUID).build()
        )
        val settings = ScanSettings.Builder()
            .setScanMode(ScanSettings.SCAN_MODE_LOW_LATENCY)
            .setCallbackType(ScanSettings.CALLBACK_TYPE_ALL_MATCHES)
            .build()
        scanner.startScan(filters, settings, callback)

        handler.postDelayed({
            try { scanner.stopScan(callback) } catch (_: Exception) {}
            val devices = JSONArray()
            for ((address, pair) in foundDevices) {
                val (result, proto) = pair
                val name = result.device.name ?: result.scanRecord?.deviceName ?: "Ratspeak peer"
                devices.put(JSONObject().apply {
                    put("name", name)
                    put("address", address)
                    put("rssi", result.rssi)
                    put("protocol", proto)
                })
            }
            sendPeerScanResult(devices = devices)
        }, timeoutMs)
    }

    private fun sendPeerScanResult(devices: JSONArray? = null, error: String? = null) {
        val json = JSONObject().apply {
            put("devices", devices ?: JSONArray())
            if (error != null) put("error", error)
        }
        handler.post {
            webViewRef?.evaluateJavascript(
                "if(typeof window._onBlePeerScanResult==='function')window._onBlePeerScanResult(${json});",
                null
            )
        }
    }

    private fun sanitizeIdentityBackupFileName(name: String): String {
        val cleaned = sanitizeIdentityDocumentFileName(name)
        return if (cleaned.endsWith(".rsi", ignoreCase = true)) cleaned else "$cleaned.rsi"
    }

    private fun sanitizeIdentityDocumentFileName(name: String): String {
        return name
            .replace(Regex("[\\\\/:*?\"<>|\\u0000-\\u001F]"), "_")
            .trim()
            .ifEmpty { "identity" }
    }

    private fun launchIdentityDocumentSave(fileName: String, bytes: ByteArray, mimeType: String?) {
        handler.post {
            try {
                pendingIdentityExport = PendingIdentityExport(fileName, bytes)
                val intent = Intent(Intent.ACTION_CREATE_DOCUMENT).apply {
                    addCategory(Intent.CATEGORY_OPENABLE)
                    type = mimeType?.takeIf { it.isNotBlank() } ?: "application/octet-stream"
                    putExtra(Intent.EXTRA_TITLE, fileName)
                }
                identityBackupDocumentLauncher.launch(intent)
            } catch (_: ActivityNotFoundException) {
                pendingIdentityExport = null
                dispatchIdentityExportResult(false, null, "No file picker available on this device")
            } catch (e: Throwable) {
                pendingIdentityExport = null
                dispatchIdentityExportResult(
                    false,
                    null,
                    e.message ?: "Unable to open save picker"
                )
            }
        }
    }

    private fun handleIdentityBackupDocumentResult(resultCode: Int, data: Intent?) {
        val pending = pendingIdentityExport
        pendingIdentityExport = null

        if (resultCode != Activity.RESULT_OK) {
            dispatchIdentityExportResult(false, null, "Export cancelled")
            return
        }

        val uri = data?.data
        if (pending == null || uri == null) {
            dispatchIdentityExportResult(false, null, "No save destination selected")
            return
        }

        Thread({
            try {
                val stream = contentResolver.openOutputStream(uri)
                    ?: throw IllegalStateException("Could not open selected destination")
                stream.use { it.write(pending.bytes) }
                dispatchIdentityExportResult(true, uri.toString(), null)
            } catch (e: Throwable) {
                dispatchIdentityExportResult(
                    false,
                    null,
                    e.message ?: "Failed to write identity backup"
                )
            }
        }, "identity-backup-export").start()
    }

    private fun dispatchIdentityExportResult(success: Boolean, uri: String?, error: String?) {
        val json = JSONObject().apply {
            put("success", success)
            if (uri != null) put("uri", uri)
            if (error != null) put("error", error)
        }
        handler.post {
            webViewRef?.evaluateJavascript(
                "if(typeof window._onAndroidIdentityExportResult==='function')window._onAndroidIdentityExportResult($json);",
                null
            )
        }
    }

    private fun sanitizeDownloadFileName(name: String, mimeType: String): String {
        val cleaned = sanitizeIdentityDocumentFileName(name)
        if (cleaned.contains('.') && cleaned.substringAfterLast('.').length in 1..8) {
            return cleaned
        }
        val ext = when (mimeType.lowercase()) {
            "image/jpeg", "image/jpg" -> "jpg"
            "image/png" -> "png"
            "image/gif" -> "gif"
            "image/webp" -> "webp"
            "image/heic" -> "heic"
            "image/heif" -> "heif"
            "image/bmp" -> "bmp"
            "application/pdf" -> "pdf"
            "text/plain" -> "txt"
            "text/csv" -> "csv"
            "application/json" -> "json"
            "application/zip" -> "zip"
            else -> ""
        }
        return if (ext.isNotEmpty()) "$cleaned.$ext" else cleaned
    }

    private fun launchGenericFileSave(
        requestId: String,
        fileName: String,
        bytes: ByteArray,
        mimeType: String
    ) {
        val safeName = sanitizeDownloadFileName(fileName, mimeType)
        handler.post {
            try {
                pendingGenericFileSave = PendingFileSave(requestId, safeName, bytes, mimeType)
                val intent = Intent(Intent.ACTION_CREATE_DOCUMENT).apply {
                    addCategory(Intent.CATEGORY_OPENABLE)
                    type = mimeType.takeIf { it.isNotBlank() } ?: "application/octet-stream"
                    putExtra(Intent.EXTRA_TITLE, safeName)
                }
                genericFileDocumentLauncher.launch(intent)
            } catch (_: ActivityNotFoundException) {
                pendingGenericFileSave = null
                dispatchFileSaveResult(requestId, false, null, "No file picker available on this device")
            } catch (e: Throwable) {
                pendingGenericFileSave = null
                dispatchFileSaveResult(requestId, false, null, e.message ?: "Unable to open save picker")
            }
        }
    }

    private fun handleGenericFileDocumentResult(resultCode: Int, data: Intent?) {
        val pending = pendingGenericFileSave
        pendingGenericFileSave = null

        if (pending == null) return
        if (resultCode != Activity.RESULT_OK) {
            dispatchFileSaveResult(pending.requestId, false, null, "Save cancelled")
            return
        }

        val uri = data?.data
        if (uri == null) {
            dispatchFileSaveResult(pending.requestId, false, null, "No save destination selected")
            return
        }

        Thread({
            try {
                val stream = contentResolver.openOutputStream(uri)
                    ?: throw IllegalStateException("Could not open selected destination")
                stream.use { it.write(pending.bytes) }
                dispatchFileSaveResult(pending.requestId, true, uri.toString(), null)
            } catch (e: Throwable) {
                dispatchFileSaveResult(
                    pending.requestId,
                    false,
                    null,
                    e.message ?: "Failed to save file"
                )
            }
        }, "ratspeak-file-save").start()
    }

    private fun saveImageToMediaStore(
        requestId: String,
        fileName: String,
        bytes: ByteArray,
        mimeType: String
    ) {
        val safeName = sanitizeDownloadFileName(fileName, mimeType)
        Thread({
            var uri: Uri? = null
            try {
                val collection = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                    MediaStore.Images.Media.getContentUri(MediaStore.VOLUME_EXTERNAL_PRIMARY)
                } else {
                    MediaStore.Images.Media.EXTERNAL_CONTENT_URI
                }
                val values = ContentValues().apply {
                    put(MediaStore.Images.Media.DISPLAY_NAME, safeName)
                    put(MediaStore.Images.Media.MIME_TYPE, mimeType.takeIf { it.isNotBlank() } ?: "image/png")
                    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                        put(MediaStore.Images.Media.RELATIVE_PATH, "Pictures/Ratspeak")
                        put(MediaStore.Images.Media.IS_PENDING, 1)
                    }
                }
                uri = contentResolver.insert(collection, values)
                    ?: throw IllegalStateException("Could not create image in Photos")
                val stream = contentResolver.openOutputStream(uri)
                    ?: throw IllegalStateException("Could not open image destination")
                stream.use { it.write(bytes) }
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                    val done = ContentValues().apply {
                        put(MediaStore.Images.Media.IS_PENDING, 0)
                    }
                    contentResolver.update(uri, done, null, null)
                }
                dispatchFileSaveResult(requestId, true, uri.toString(), null)
            } catch (e: Throwable) {
                if (uri != null) {
                    try { contentResolver.delete(uri, null, null) } catch (_: Throwable) {}
                }
                dispatchFileSaveResult(
                    requestId,
                    false,
                    null,
                    e.message ?: "Failed to save image"
                )
            }
        }, "ratspeak-photo-save").start()
    }

    private fun dispatchFileSaveResult(
        requestId: String,
        success: Boolean,
        uri: String?,
        error: String?
    ) {
        val json = JSONObject().apply {
            put("request_id", requestId)
            put("success", success)
            if (uri != null) put("uri", uri)
            if (error != null) put("error", error)
        }
        handler.post {
            webViewRef?.evaluateJavascript(
                "if(typeof window._onAndroidFileSaveResult==='function')window._onAndroidFileSaveResult($json);",
                null
            )
        }
    }

    private fun handleIdentityImportDocumentResult(resultCode: Int, data: Intent?) {
        if (resultCode != Activity.RESULT_OK) {
            dispatchIdentityImportResult(false, null, null, null, null, "Import cancelled")
            return
        }

        val uri = data?.data
        if (uri == null) {
            dispatchIdentityImportResult(false, null, null, null, null, "No identity backup selected")
            return
        }

        Thread({
            try {
                val bytes = readIdentityImportBytes(uri)
                val fileName = displayNameForUri(uri) ?: "identity backup"
                val b64 = Base64.encodeToString(bytes, Base64.NO_WRAP)
                dispatchIdentityImportResult(
                    true,
                    fileName,
                    bytes.size,
                    b64,
                    uri.toString(),
                    null
                )
            } catch (e: Throwable) {
                dispatchIdentityImportResult(
                    false,
                    null,
                    null,
                    null,
                    null,
                    e.message ?: "Failed to read identity backup"
                )
            }
        }, "identity-backup-import").start()
    }

    private fun readIdentityImportBytes(uri: Uri): ByteArray {
        val stream = contentResolver.openInputStream(uri)
            ?: throw IllegalStateException("Could not open selected identity backup")
        stream.use { input ->
            val out = ByteArrayOutputStream()
            val buf = ByteArray(8192)
            var total = 0
            while (true) {
                val read = input.read(buf)
                if (read < 0) break
                total += read
                if (total > MAX_IDENTITY_IMPORT_BYTES) {
                    throw IllegalStateException("Identity backup is too large")
                }
                out.write(buf, 0, read)
            }
            return out.toByteArray()
        }
    }

    private fun displayNameForUri(uri: Uri): String? {
        return try {
            contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)
                ?.use { cursor ->
                    if (!cursor.moveToFirst()) return@use null
                    val idx = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME)
                    if (idx >= 0) cursor.getString(idx) else null
                }
        } catch (_: Throwable) {
            null
        } ?: uri.lastPathSegment?.substringAfterLast('/')
    }

    private fun dispatchIdentityImportResult(
        success: Boolean,
        fileName: String?,
        fileSize: Int?,
        backupBase64: String?,
        uri: String?,
        error: String?
    ) {
        val json = JSONObject().apply {
            put("success", success)
            if (fileName != null) put("file_name", fileName)
            if (fileSize != null) put("file_size", fileSize)
            if (backupBase64 != null) put("backup_base64", backupBase64)
            if (uri != null) put("uri", uri)
            if (error != null) put("error", error)
        }
        handler.post {
            webViewRef?.evaluateJavascript(
                "if(typeof window._onAndroidIdentityImportResult==='function')window._onAndroidIdentityImportResult($json);",
                null
            )
        }
    }

    /**
     * JavaScript interface exposed to the WebView as window.RatspeakAndroid.
     * Provides BLE permission requests and native BLE scanning using modern
     * BluetoothManager API (works on Android 13–16+).
     */
    inner class BlePermissionBridge {
        @JavascriptInterface
        fun exportIdentityBackup(fileName: String, backupBase64: String) {
            val safeName = sanitizeIdentityBackupFileName(fileName)
            val bytes = try {
                Base64.decode(backupBase64, Base64.DEFAULT)
            } catch (_: Throwable) {
                dispatchIdentityExportResult(false, null, "Invalid identity backup data")
                return
            }

            // The payload is a JSON envelope, but the public file type is
            // Ratspeak's .rsi backup. Android document providers commonly
            // append ".json" to application/json save targets, producing
            // confusing .rsi.json names.
            launchIdentityDocumentSave(safeName, bytes, "application/octet-stream")
        }

        @JavascriptInterface
        fun saveIdentityDocument(fileName: String, dataBase64: String, mimeType: String) {
            val safeName = sanitizeIdentityDocumentFileName(fileName)
            val bytes = try {
                Base64.decode(dataBase64, Base64.DEFAULT)
            } catch (_: Throwable) {
                dispatchIdentityExportResult(false, null, "Invalid identity export data")
                return
            }

            launchIdentityDocumentSave(safeName, bytes, mimeType)
        }

        @JavascriptInterface
        fun saveFileDocument(
            fileName: String,
            dataBase64: String,
            mimeType: String,
            requestId: String
        ) {
            val bytes = try {
                Base64.decode(dataBase64, Base64.DEFAULT)
            } catch (_: Throwable) {
                dispatchFileSaveResult(requestId, false, null, "Invalid file data")
                return
            }
            launchGenericFileSave(
                requestId,
                fileName,
                bytes,
                mimeType.ifBlank { "application/octet-stream" }
            )
        }

        @JavascriptInterface
        fun saveImageToPhotos(
            fileName: String,
            dataBase64: String,
            mimeType: String,
            requestId: String
        ) {
            val bytes = try {
                Base64.decode(dataBase64, Base64.DEFAULT)
            } catch (_: Throwable) {
                dispatchFileSaveResult(requestId, false, null, "Invalid image data")
                return
            }
            saveImageToMediaStore(
                requestId,
                fileName,
                bytes,
                mimeType.takeIf { it.startsWith("image/", ignoreCase = true) } ?: "image/png"
            )
        }

        @JavascriptInterface
        fun openExternalUrl(url: String): Boolean {
            val parsed = try { Uri.parse(url.trim()) } catch (_: Throwable) { return false }
            val scheme = parsed.scheme?.lowercase() ?: return false
            if (scheme != "http" && scheme != "https") return false
            val intent = Intent(Intent.ACTION_VIEW, parsed).apply {
                addCategory(Intent.CATEGORY_BROWSABLE)
            }
            return try {
                startActivity(intent)
                true
            } catch (_: Throwable) {
                false
            }
        }

        @JavascriptInterface
        fun importIdentityBackup() {
            handler.post {
                try {
                    val intent = Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
                        addCategory(Intent.CATEGORY_OPENABLE)
                        // Do not filter by MIME here. Android document providers
                        // report .rsi files as application/json, octet-stream, or
                        // vendor-specific types depending on where they were saved.
                        // The Rust preview/import path validates the bytes.
                        type = "*/*"
                    }
                    identityImportDocumentLauncher.launch(intent)
                } catch (_: ActivityNotFoundException) {
                    dispatchIdentityImportResult(
                        false,
                        null,
                        null,
                        null,
                        null,
                        "No file picker available on this device"
                    )
                } catch (e: Throwable) {
                    dispatchIdentityImportResult(
                        false,
                        null,
                        null,
                        null,
                        null,
                        e.message ?: "Unable to open identity backup picker"
                    )
                }
            }
        }

        @JavascriptInterface
        fun requestBlePermissions() {
            if (hasBlePermissions()) {
                // Already granted — notify immediately
                handler.post {
                    webViewRef?.evaluateJavascript(
                        "if(typeof window._onBlePermissionResult==='function')window._onBlePermissionResult(true);",
                        null
                    )
                }
                return
            }
            handler.post {
                ActivityCompat.requestPermissions(
                    this@MainActivity,
                    getBlePermissions(),
                    BLE_PERMISSION_REQUEST_CODE
                )
            }
        }

        @JavascriptInterface
        fun hasBlePermissions(): Boolean {
            return this@MainActivity.hasBlePermissions()
        }

        @JavascriptInterface
        fun hasMediaPermissions(audio: Boolean, camera: Boolean): Boolean {
            return this@MainActivity.hasMediaPermissions(audio, camera)
        }

        @JavascriptInterface
        fun requestMediaPermissions(audio: Boolean, camera: Boolean, requestId: String) {
            val permissions = getMediaPermissions(audio, camera)
            if (permissions.isEmpty() || this@MainActivity.hasMediaPermissions(audio, camera)) {
                dispatchMediaPermissionResult(requestId, audio, camera, true, null)
                return
            }
            handler.post {
                pendingMediaRequestId = requestId
                pendingMediaRequestAudio = audio
                pendingMediaRequestCamera = camera
                ActivityCompat.requestPermissions(
                    this@MainActivity,
                    permissions,
                    MEDIA_PERMISSION_REQUEST_CODE
                )
            }
        }

        @JavascriptInterface
        fun getQrScannerEnvironment(): String {
            return this@MainActivity.buildQrScannerEnvironment()
        }

        @JavascriptInterface
        fun playCallRingtone(mode: String): Boolean {
            return this@MainActivity.runOnMainForBoolean {
                this@MainActivity.startNativeCallRingtone(mode)
            }
        }

        @JavascriptInterface
        fun stopCallRingtone() {
            handler.post {
                this@MainActivity.stopNativeCallRingtone()
            }
        }

        @JavascriptInterface
        fun startCallAudioRoute(role: String) {
            handler.post {
                this@MainActivity.startNativeCallAudioRoute(role)
            }
        }

        @JavascriptInterface
        fun stopCallAudioRoute() {
            handler.post {
                this@MainActivity.stopNativeCallAudioRoute()
            }
        }

        /**
         * Start a native BLE scan. Results are delivered via window._onNativeBleScanResult(data).
         * This uses BluetoothManager (modern API), not the deprecated getDefaultAdapter().
         */
        @JavascriptInterface
        fun scanBleDevices(timeoutMs: Long) {
            if (!this@MainActivity.hasBlePermissions()) {
                sendBleScanResult(error = "Bluetooth permissions not granted")
                return
            }
            handler.post {
                startNativeBleScan(timeoutMs)
            }
        }

        /**
         * Connect to a BLE device and start the TCP bridge.
         * Result delivered via window._onBleConnectResult(json).
         * On success, json.port contains the local TCP port for Rust to connect to.
         */
        @JavascriptInterface
        fun connectBleDevice(address: String, localPort: Int) {
            if (!BLE_MAC_RE.matches(address)) {
                // Bail before touching BluetoothAdapter.getRemoteDevice, which
                // would throw IllegalArgumentException buried in Logcat. A
                // structured early error makes the frontend able to show a
                // meaningful toast.
                val errJson = JSONObject()
                    .put("success", false)
                    .put("port", localPort)
                    .put("error", "Invalid BLE address format (expected XX:XX:XX:XX:XX:XX)")
                handler.post {
                    webViewRef?.evaluateJavascript(
                        "if(typeof window._onBleConnectResult==='function')window._onBleConnectResult($errJson);",
                        null
                    )
                }
                return
            }
            Thread({
                // Disconnect any existing connection
                bleGatt?.disconnect()
                val gatt = RatspeakBleGatt(this@MainActivity)
                bleGatt = gatt
                // Let the bridge push phase updates to JS during the multi-step connect.
                gatt.attachWebView(webViewRef)

                val error = gatt.connect(address, localPort)
                if (error != null) {
                    gatt.disconnect()
                    if (bleGatt === gatt) bleGatt = null
                }
                val result = JSONObject().apply {
                    put("success", error == null)
                    put("port", localPort)
                    if (error != null) put("error", error)
                }
                handler.post {
                    webViewRef?.evaluateJavascript(
                        "if(typeof window._onBleConnectResult==='function')window._onBleConnectResult($result);",
                        null
                    )
                }

                // If connection succeeded, start forwarding (blocks until disconnected)
                if (error == null) {
                    gatt.startForwarding()
                }
            }, "ble-gatt-connect").start()
        }

        /**
         * Disconnect the active BLE GATT connection and tear down the TCP bridge.
         */
        @JavascriptInterface
        fun disconnectBleDevice() {
            Thread({
                bleGatt?.disconnect()
                bleGatt = null
            }, "ble-gatt-disconnect").start()
        }

        // ---- Bluetooth Peer bridge (Bitchat-style symmetric peering) ----
        //
        // These mirror the RNode flow but: (1) filter on Ratspeak/Columba
        // service UUIDs, (2) skip bonding, (3) wire data through Rust JNI
        // instead of a TCP bridge. The actual GATT lifecycle lives in
        // RatspeakBlePeerClient; the Rust runtime owns the connection
        // policy and per-peer state.

        /**
         * Open the Bluetooth Peer GATT server and start advertising. Returns
         * synchronously via the JS bridge — startup is fast (< 200 ms).
         * The Rust [android_peripheral::start_advertising] path also calls
         * into RatspeakBleServer / openGattServer; this JS bridge is for
         * cases where the peer mode is toggled directly from the WebView
         * (e.g., the Network → Bluetooth modal) without going through the
         * Rust runtime spawn path first.
         */
        @JavascriptInterface
        fun startBlePeerMode(identityHashHex: String): Boolean {
            if (!this@MainActivity.hasBlePermissions()) return false
            val id = try { hexToBytes(identityHashHex) } catch (_: Throwable) { return false }
            if (id.size != 16) return false
            return RatspeakBleServer.openGattServer(this@MainActivity, id)
        }

        /**
         * Stop Bluetooth Peer mode (close the GATT server). Idempotent.
         */
        @JavascriptInterface
        fun stopBlePeerMode() {
            RatspeakBleServer.closeGattServer()
        }

        /**
         * Scan for nearby Ratspeak / Columba peers. Results delivered via
         * window._onBlePeerScanResult(json) where json.devices is an array
         * of {name, address, rssi, protocol}.
         */
        @JavascriptInterface
        fun scanForBlePeers(timeoutMs: Long) {
            if (!this@MainActivity.hasBlePermissions()) {
                sendPeerScanResult(error = "Bluetooth permissions not granted")
                return
            }
            handler.post { startPeerBleScan(timeoutMs) }
        }

        /**
         * Connect to a peer at `address` as Central. On success, the peer's
         * TX notifications are wired to Rust via JNI; identity flows in
         * later via the first signed Reticulum announce (Bitchat-style).
         * Result delivered via
         * window._onBlePeerConnectResult({address, success, error?}).
         */
        @JavascriptInterface
        fun connectToBlePeer(address: String) {
            if (!BLE_MAC_RE.matches(address)) {
                val errJson = JSONObject()
                    .put("address", address)
                    .put("success", false)
                    .put("error", "Invalid BLE address format (expected XX:XX:XX:XX:XX:XX)")
                handler.post {
                    webViewRef?.evaluateJavascript(
                        "if(typeof window._onBlePeerConnectResult==='function')window._onBlePeerConnectResult($errJson);",
                        null
                    )
                }
                return
            }
            Thread({
                val client = RatspeakBlePeerClient(this@MainActivity)
                val ok = client.connect(address)
                val result = JSONObject().apply {
                    put("address", address)
                    put("success", ok)
                    if (!ok) put("error", "Connect failed (see Logcat)")
                }
                handler.post {
                    webViewRef?.evaluateJavascript(
                        "if(typeof window._onBlePeerConnectResult==='function')window._onBlePeerConnectResult($result);",
                        null
                    )
                }
            }, "ble-peer-connect").start()
        }

        /**
         * Disconnect a specific peer by address. Idempotent — safe to call
         * for an address with no live client.
         */
        @JavascriptInterface
        fun disconnectBlePeer(address: String) {
            Thread({
                RatspeakBlePeerClient.disconnect(address)
            }, "ble-peer-disconnect").start()
        }

        private fun hexToBytes(hex: String): ByteArray {
            val clean = hex.removePrefix("0x").lowercase()
            require(clean.length % 2 == 0)
            return ByteArray(clean.length / 2) { i ->
                val hi = Character.digit(clean[i * 2], 16)
                val lo = Character.digit(clean[i * 2 + 1], 16)
                require(hi >= 0 && lo >= 0)
                ((hi shl 4) or lo).toByte()
            }
        }

        private fun bytesToHex(b: ByteArray): String {
            val sb = StringBuilder(b.size * 2)
            for (byte in b) sb.append("%02x".format(byte.toInt() and 0xFF))
            return sb.toString()
        }

        // ---- USB-OTG permission bridge ----
        //
        // USB permissions on Android are per-app + per-device and must be
        // requested via PendingIntent+BroadcastReceiver on the Activity.
        // Rust-side JNI cannot do this itself. The flow is:
        //   1. JS calls hasUsbPermission(deviceName) — synchronous probe.
        //   2. If false, JS calls requestUsbPermission(deviceName).
        //   3. The system shows a permission dialog.
        //   4. We broadcast the result back via window._onUsbPermissionResult.
        //   5. JS then posts /api/android/usb/connect to the Rust backend,
        //      which claims the device via JNI (see android_usb.rs).

        @JavascriptInterface
        fun hasUsbPermission(deviceName: String): Boolean {
            val um = getSystemService(Context.USB_SERVICE) as? UsbManager ?: return false
            val device = um.deviceList[deviceName] ?: return false
            return um.hasPermission(device)
        }

        @JavascriptInterface
        fun requestUsbPermission(deviceName: String) {
            handler.post {
                val um = getSystemService(Context.USB_SERVICE) as? UsbManager
                if (um == null) {
                    dispatchUsbResult(deviceName, false, "USB service unavailable")
                    return@post
                }
                val device = um.deviceList[deviceName]
                if (device == null) {
                    dispatchUsbResult(deviceName, false, "Device not found: $deviceName")
                    return@post
                }
                if (um.hasPermission(device)) {
                    dispatchUsbResult(deviceName, true, null)
                    return@post
                }

                // Register a one-shot receiver if we don't already have one.
                if (usbPermissionReceiver == null) {
                    val receiver = object : BroadcastReceiver() {
                        override fun onReceive(ctx: Context, intent: Intent) {
                            if (intent.action != USB_PERMISSION_ACTION) return
                            val d: UsbDevice? = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                                intent.getParcelableExtra(UsbManager.EXTRA_DEVICE, UsbDevice::class.java)
                            } else {
                                @Suppress("DEPRECATION")
                                intent.getParcelableExtra(UsbManager.EXTRA_DEVICE)
                            }
                            val granted = intent.getBooleanExtra(UsbManager.EXTRA_PERMISSION_GRANTED, false)
                            val name = d?.deviceName ?: ""
                            dispatchUsbResult(name, granted, null)
                        }
                    }
                    val filter = IntentFilter(USB_PERMISSION_ACTION)
                    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                        registerReceiver(receiver, filter, Context.RECEIVER_NOT_EXPORTED)
                    } else {
                        registerReceiver(receiver, filter)
                    }
                    usbPermissionReceiver = receiver
                }

                val pendingFlags = PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
                val permIntent = Intent(USB_PERMISSION_ACTION).setPackage(packageName)
                val pending = PendingIntent.getBroadcast(this@MainActivity, 0, permIntent, pendingFlags)
                um.requestPermission(device, pending)
            }
        }

        @JavascriptInterface
        fun listUsbDevices(): String {
            // Mirror android_usb::enumerate_usb_devices, but expose to JS
            // directly so the modal can show a device list without a round
            // trip through the Rust backend.
            val um = getSystemService(Context.USB_SERVICE) as? UsbManager
                ?: return "[]"
            val arr = JSONArray()
            for ((name, dev) in um.deviceList) {
                val obj = JSONObject().apply {
                    put("device_name", name)
                    put("vid", dev.vendorId)
                    put("pid", dev.productId)
                    put("manufacturer", dev.manufacturerName ?: "")
                    put("product", dev.productName ?: "")
                    put("has_permission", um.hasPermission(dev))
                }
                arr.put(obj)
            }
            return arr.toString()
        }
    }

    /** Post a USB permission result to the WebView. */
    private fun dispatchUsbResult(deviceName: String, granted: Boolean, error: String?) {
        val json = JSONObject().apply {
            put("device_name", deviceName)
            put("granted", granted)
            if (error != null) put("error", error)
        }
        handler.post {
            webViewRef?.evaluateJavascript(
                "if(typeof window._onUsbPermissionResult==='function')window._onUsbPermissionResult($json);",
                null
            )
        }
    }

    private fun dispatchMediaPermissionResult(
        requestId: String,
        audio: Boolean,
        camera: Boolean,
        granted: Boolean,
        error: String?
    ) {
        val json = JSONObject().apply {
            put("request_id", requestId)
            put("audio", audio)
            put("camera", camera)
            put("granted", granted)
            if (error != null) put("error", error)
        }
        handler.post {
            webViewRef?.evaluateJavascript(
                "if(typeof window._onAndroidMediaPermissionResult==='function')window._onAndroidMediaPermissionResult($json);",
                null
            )
        }
    }
}
