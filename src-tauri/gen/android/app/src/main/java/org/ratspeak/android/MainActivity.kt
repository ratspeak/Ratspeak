package org.ratspeak.android

import android.Manifest
import android.annotation.SuppressLint
import android.app.Activity
import android.app.NotificationManager
import android.bluetooth.BluetoothDevice
import android.bluetooth.BluetoothManager
import android.bluetooth.le.BluetoothLeScanner
import android.bluetooth.le.ScanCallback
import android.bluetooth.le.ScanResult
import android.bluetooth.le.ScanSettings
import android.content.ActivityNotFoundException
import android.content.ContentValues
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.media.AudioAttributes
import android.media.AudioDeviceInfo
import android.media.AudioFocusRequest
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioTrack
import android.net.Uri
import android.hardware.usb.UsbManager
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
import androidx.core.content.edit
import androidx.core.graphics.toColorInt
import androidx.core.net.toUri
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.webkit.WebViewCompat
import org.json.JSONArray
import org.json.JSONObject
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.FileInputStream
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlin.math.PI
import kotlin.math.cos
import kotlin.math.pow
import kotlin.math.sin

class MainActivity : TauriActivity() {
    companion object {
        private const val BLE_PERMISSION_REQUEST_CODE = 1001
        private const val MEDIA_PERMISSION_REQUEST_CODE = 1003
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
        private const val CALL_TIMEOUT_CUE_MS = 520L
        private const val CALL_TIMEOUT_CUE_VOLUME = 0.20
        private const val CALL_TIMEOUT_CUE_GLIDE_CENTS = -6.0
        private const val CALL_TIMEOUT_CUE_ATTACK_MS = 7L
        private const val CALL_TIMEOUT_CUE_RELEASE_MS = 58L
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
        private val CALL_TIMEOUT_CUE_START_MS = longArrayOf(0L, 112L, 238L)
        private val CALL_TIMEOUT_CUE_FREQ_HZ = doubleArrayOf(
            CALL_RINGTONE_B5_HZ,
            CALL_RINGTONE_G5_HZ,
            CALL_RINGTONE_E5_HZ
        )
        private val CALL_TIMEOUT_CUE_DURATION_MS = longArrayOf(88L, 104L, 168L)
        private val CALL_TIMEOUT_CUE_NOTE_GAIN = doubleArrayOf(0.82, 0.74, 0.68)
        private val CALL_RINGTONE_INCOMING_PARTIALS = doubleArrayOf(0.74, 0.18, 0.08)
        private val CALL_RINGTONE_OUTGOING_PARTIALS = doubleArrayOf(0.80, 0.15, 0.05)
        // Standard Bluetooth MAC-48 address format: 6 hex octets separated
        // by colons. Used to guard the BLE connect bridge methods before we
        // hand the string to BluetoothAdapter.getRemoteDevice, which throws
        // IllegalArgumentException on malformed input.
        private val BLE_MAC_RE = Regex("^([0-9A-Fa-f]{2}:){5}[0-9A-Fa-f]{2}$")
        private val BLE_OPERATION_RE = Regex("^[0-9A-Fa-f]{32}$")
    }
    private var webViewRef: WebView? = null
    private var appBackCallback: OnBackPressedCallback? = null
    private val handler = Handler(Looper.getMainLooper())
    private var pendingTop = 0
    private var pendingBottom = 0
    private var pendingNavigate: String? = null
    private var pendingIdentityExport: PendingIdentityExport? = null
    private var pendingGenericFileSave: PendingFileSave? = null
    private var pendingMediaRequestId: String? = null
    private var pendingMediaRequestAudio = false
    private var pendingMediaRequestCamera = false
    private var callRingtoneGeneration = 0
    private var callRingtoneMode: String? = null
    private var callRingtoneTrack: AudioTrack? = null
    private var callRingtoneFocusRequest: Any? = null
    private var voiceMemoAudioFocusRequest: Any? = null
    private val callRingtoneFocusListener = AudioManager.OnAudioFocusChangeListener { change ->
        if (change == AudioManager.AUDIOFOCUS_LOSS || change == AudioManager.AUDIOFOCUS_LOSS_TRANSIENT) {
            handler.post { stopNativeCallRingtone() }
        }
    }
    private val voiceMemoAudioFocusListener = AudioManager.OnAudioFocusChangeListener { change ->
        if (change == AudioManager.AUDIOFOCUS_LOSS ||
            change == AudioManager.AUDIOFOCUS_LOSS_TRANSIENT) {
            handler.post { dispatchVoiceMemoAudioInterruption() }
        }
    }

    private data class PendingIdentityExport(val fileName: String, val bytes: ByteArray)
    private data class PendingFileSave(
        val requestId: String,
        val fileName: String,
        val bytes: ByteArray?,
        val privateFile: File?,
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
        // Incoming call ringtones are app audio, not microphone capture.
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
        // Tauri setup can start Rust and restore saved BLE interfaces inside
        // super.onCreate(), so install the Application context first.
        RatspeakNativeBridge.initialize(applicationContext)
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)
        RatspeakAndroidObservers.attach(this)

        // Check for notification navigation intent
        handleNavigateIntent(intent)

        // Match splash background to OS theme preference
        val isDarkMode = (resources.configuration.uiMode and
            android.content.res.Configuration.UI_MODE_NIGHT_MASK) ==
            android.content.res.Configuration.UI_MODE_NIGHT_YES
        val bgColor = if (isDarkMode) "#18171a" else "#FAF7F3"
        window.decorView.setBackgroundColor(bgColor.toColorInt())

        setTransparentSystemBars()

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
        ContextCompat.startForegroundService(this, serviceIntent)

    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handleNavigateIntent(intent)
    }

    override fun onTrimMemory(level: Int) {
        super.onTrimMemory(level)
        // UI_HIDDEN (20) is a lifecycle edge, not memory pressure; treating
        // it as pressure would cancel staging while the system picker opens.
        RatspeakMobilePolicy.attachmentMemoryPressure(level)?.let {
            RatspeakNativeBridge.publishMemoryPressure(it)
        }
    }

    override fun onLowMemory() {
        super.onLowMemory()
        RatspeakNativeBridge.publishMemoryPressure(true)
    }

    override fun onResume() {
        super.onResume()
        RatspeakPlatformSupervisor.replay()
        // ACTION_REFRESH clears per-sender notifications in RatspeakService
        // and kicks the poll loop so lastKnownUnread is current before the
        // user reads messages to zero.
        refreshServicePoll()
    }

    override fun onPause() {
        super.onPause()
        stopNativeVoiceMemoAudioSession()
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

    @Suppress("DEPRECATION")
    private fun setTransparentSystemBars() {
        // Both bars transparent; WebView CSS renders the safe areas.
        window.statusBarColor = android.graphics.Color.TRANSPARENT
        window.navigationBarColor = android.graphics.Color.TRANSPARENT
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            window.isNavigationBarContrastEnforced = false
        }
    }

    private fun applySystemBarColorMode(mode: String) {
        if (mode != "light" && mode != "dark") return
        val isLight = mode == "light"
        handler.post {
            WindowCompat.getInsetsController(window, window.decorView).apply {
                isAppearanceLightStatusBars = isLight
                isAppearanceLightNavigationBars = isLight
            }
        }
    }

    override fun onDestroy() {
        RatspeakAndroidObservers.detach(this)
        // Ringtone and voice-memo sessions are UI-owned. Established-call
        // routing and playback are process-owned and intentionally survive an
        // Activity recreation.
        stopNativeCallRingtone()
        stopNativeVoiceMemoAudioSession()
        RatspeakCallAudio.cancelInteractivePrime(this)
        if (!RatspeakCallAudio.isActive()) RatspeakVoiceAudio.stop()
        super.onDestroy()
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
     * Runs every 3s for 30s after page load as an initialization fallback.
     * Later user changes arrive immediately through setColorMode().
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
                    applySystemBarColorMode(theme)
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
        return when {
            mode.equals("incoming", ignoreCase = true) -> "incoming"
            mode.equals("timeout", ignoreCase = true) -> "timeout"
            else -> "outgoing"
        }
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
        if (!RatspeakCallAudio.isActive()) {
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

    private fun startNativeVoiceMemoAudioSession(): Boolean {
        if (RatspeakCallAudio.isActive() || callRingtoneMode != null) return false
        val audioManager = getSystemService(Context.AUDIO_SERVICE) as? AudioManager ?: return false
        val result = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val attributes = AudioAttributes.Builder()
                .setUsage(AudioAttributes.USAGE_ASSISTANT)
                .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
                .build()
            val existing = voiceMemoAudioFocusRequest as? AudioFocusRequest
            if (existing != null) return true
            val request = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN_TRANSIENT_EXCLUSIVE)
                .setAudioAttributes(attributes)
                .setOnAudioFocusChangeListener(voiceMemoAudioFocusListener, handler)
                .build()
            val focusResult = audioManager.requestAudioFocus(request)
            if (focusResult == AudioManager.AUDIOFOCUS_REQUEST_GRANTED) {
                voiceMemoAudioFocusRequest = request
            }
            focusResult
        } else {
            @Suppress("DEPRECATION")
            audioManager.requestAudioFocus(
                voiceMemoAudioFocusListener,
                AudioManager.STREAM_MUSIC,
                AudioManager.AUDIOFOCUS_GAIN_TRANSIENT_EXCLUSIVE
            )
        }
        return result == AudioManager.AUDIOFOCUS_REQUEST_GRANTED
    }

    private fun stopNativeVoiceMemoAudioSession() {
        val audioManager = getSystemService(Context.AUDIO_SERVICE) as? AudioManager ?: return
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val request = voiceMemoAudioFocusRequest as? AudioFocusRequest
            if (request != null) {
                audioManager.abandonAudioFocusRequest(request)
                voiceMemoAudioFocusRequest = null
                return
            }
        }
        @Suppress("DEPRECATION")
        audioManager.abandonAudioFocus(voiceMemoAudioFocusListener)
    }

    private fun dispatchVoiceMemoAudioInterruption() {
        webViewRef?.evaluateJavascript(
            "window.RS && window.RS.voiceMemos && window.RS.voiceMemos.handleAudioInterruption && window.RS.voiceMemos.handleAudioInterruption();",
            null
        )
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
            if (!RatspeakCallAudio.isActive()) {
                restoreCallAudioRoute()
                audioManager.mode = AudioManager.MODE_RINGTONE
            }
            return
        }
        if (!RatspeakCallAudio.isActive()) {
            configureCommunicationRoute(preferEarpiece = true)
        }
    }

    private fun primeNativeCallAudioRoute(role: String) {
        stopNativeVoiceMemoAudioSession()
        volumeControlStream = AudioManager.STREAM_VOICE_CALL
        if (!RatspeakCallAudio.primeInteractive(applicationContext, role)) {
            volumeControlStream = AudioManager.USE_DEFAULT_STREAM_TYPE
            Log.d("Ratspeak", "LXST pending call audio route could not start")
        }
    }

    private fun updateNativeCallAudioRoute(role: String, sessionToken: String) {
        volumeControlStream = AudioManager.STREAM_VOICE_CALL
        if (!RatspeakCallAudio.updateRouteForSession(applicationContext, sessionToken, role)) {
            volumeControlStream = AudioManager.USE_DEFAULT_STREAM_TYPE
            Log.d("Ratspeak", "Rejected stale LXST call audio route update")
        }
    }

    private fun stopNativeCallAudioRoute(waitForNoProximity: Boolean = true) {
        RatspeakCallAudio.stop(applicationContext, waitForNoProximity)
        volumeControlStream = AudioManager.USE_DEFAULT_STREAM_TYPE
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

    private fun callRingtoneSequenceMs(mode: String): Long {
        return if (mode == "timeout") CALL_TIMEOUT_CUE_MS else CALL_RINGTONE_LOOP_MS
    }

    private fun callRingtoneNoteCount(mode: String): Int {
        return when (mode) {
            "incoming" -> CALL_RINGTONE_INCOMING_FREQ_HZ.size
            "timeout" -> CALL_TIMEOUT_CUE_FREQ_HZ.size
            else -> CALL_RINGTONE_OUTGOING_FREQ_HZ.size
        }
    }

    private fun callRingtoneNoteStartMs(mode: String, noteIndex: Int): Long {
        return when (mode) {
            "incoming" -> CALL_RINGTONE_INCOMING_START_MS[noteIndex]
            "timeout" -> CALL_TIMEOUT_CUE_START_MS[noteIndex]
            else -> CALL_RINGTONE_OUTGOING_START_MS[noteIndex]
        }
    }

    private fun callRingtoneNoteFrequency(mode: String, noteIndex: Int): Double {
        return when (mode) {
            "incoming" -> CALL_RINGTONE_INCOMING_FREQ_HZ[noteIndex]
            "timeout" -> CALL_TIMEOUT_CUE_FREQ_HZ[noteIndex]
            else -> CALL_RINGTONE_OUTGOING_FREQ_HZ[noteIndex]
        }
    }

    private fun callRingtoneNoteDurationMs(mode: String, noteIndex: Int): Long {
        return when (mode) {
            "incoming" -> CALL_RINGTONE_INCOMING_DURATION_MS[noteIndex]
            "timeout" -> CALL_TIMEOUT_CUE_DURATION_MS[noteIndex]
            else -> CALL_RINGTONE_OUTGOING_DURATION_MS[noteIndex]
        }
    }

    private fun callRingtoneNoteGain(mode: String, noteIndex: Int): Double {
        return when (mode) {
            "incoming" -> CALL_RINGTONE_INCOMING_NOTE_GAIN[noteIndex]
            "timeout" -> CALL_TIMEOUT_CUE_NOTE_GAIN[noteIndex]
            else -> CALL_RINGTONE_OUTGOING_NOTE_GAIN[noteIndex]
        }
    }

    private fun callRingtonePartials(mode: String): DoubleArray {
        return if (mode == "incoming") CALL_RINGTONE_INCOMING_PARTIALS
        else CALL_RINGTONE_OUTGOING_PARTIALS
    }

    private fun callRingtoneVolume(mode: String): Double {
        return when (mode) {
            "incoming" -> CALL_RINGTONE_INCOMING_VOLUME
            "timeout" -> CALL_TIMEOUT_CUE_VOLUME
            else -> CALL_RINGTONE_OUTGOING_VOLUME
        }
    }

    private fun callRingtoneGlideCents(mode: String): Double {
        return when (mode) {
            "incoming" -> CALL_RINGTONE_INCOMING_GLIDE_CENTS
            "timeout" -> CALL_TIMEOUT_CUE_GLIDE_CENTS
            else -> CALL_RINGTONE_OUTGOING_GLIDE_CENTS
        }
    }

    private fun callRingtoneAttackMs(mode: String): Long {
        return when (mode) {
            "incoming" -> CALL_RINGTONE_INCOMING_ATTACK_MS
            "timeout" -> CALL_TIMEOUT_CUE_ATTACK_MS
            else -> CALL_RINGTONE_OUTGOING_ATTACK_MS
        }
    }

    private fun callRingtoneReleaseMs(mode: String): Long {
        return when (mode) {
            "incoming" -> CALL_RINGTONE_INCOMING_RELEASE_MS
            "timeout" -> CALL_TIMEOUT_CUE_RELEASE_MS
            else -> CALL_RINGTONE_OUTGOING_RELEASE_MS
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
            if (mode != "timeout") {
                track.setLoopPoints(0, frameCount, -1)
            }
            callRingtoneTrack = track
            track.play()
            if (mode == "timeout") {
                handler.postDelayed({
                    if (callRingtoneGeneration == generation && callRingtoneMode == "timeout") {
                        stopNativeCallRingtone()
                    }
                }, CALL_TIMEOUT_CUE_MS + 80L)
            }
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
        val totalSamples = ((CALL_RINGTONE_SAMPLE_RATE * callRingtoneSequenceMs(mode)) / 1000L)
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
        } else if (requestCode == 1002) {
            handler.post {
                webViewRef?.evaluateJavascript(
                    "document.dispatchEvent(new CustomEvent('rs-notification-permission-changed'));",
                    null,
                )
            }
        }
    }

    // ---- Native BLE scanner (modern BluetoothManager API) ----

    private var bleScanner: BluetoothLeScanner? = null
    private var bleScanCallback: ScanCallback? = null

    // Nordic UART Service UUID — shared with RatspeakBleGatt + Rust side (see BleUuids.kt).
    private val NUS_SERVICE_UUID = BleUuids.NUS_SERVICE_PARCEL

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
                // NUS is sufficient protocol evidence, including custom-named
                // RNode firmware. Retain the name only as an advertisement
                // fallback when the service list is absent.
                val hasNus = serviceUuids.contains(NUS_SERVICE_UUID)
                val nameMatch = name.startsWith("RNode")
                val isRnode = hasNus || (serviceUuids.isEmpty() && nameMatch)
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
                pendingGenericFileSave = PendingFileSave(requestId, safeName, bytes, null, mimeType)
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
                stream.use { output ->
                    val bytes = pending.bytes
                    if (bytes != null) {
                        output.write(bytes)
                    } else {
                        FileInputStream(pending.privateFile ?: error("Private file is unavailable"))
                            .use { input -> input.copyTo(output, 64 * 1024) }
                    }
                }
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

    internal fun onSaveStoredFile(
        privatePath: String,
        fileName: String,
        mimeType: String,
        preferPhotos: Boolean,
        requestId: String,
    ): Boolean {
        if (!Regex("^[A-Za-z0-9._-]{1,128}$").matches(requestId)) return false
        if (mimeType.length > 200 || mimeType.any { it < ' ' }) return false
        val source = try { File(privatePath).canonicalFile } catch (_: Throwable) { return false }
        val privateRoot = try { filesDir.canonicalFile } catch (_: Throwable) { return false }
        val privatePrefix = privateRoot.path + File.separator
        if (!source.isFile || !source.path.startsWith(privatePrefix)) return false
        val safeName = sanitizeDownloadFileName(fileName, mimeType)
        if (preferPhotos && mimeType.startsWith("image/", ignoreCase = true)) {
            saveStoredImageToMediaStore(requestId, safeName, source, mimeType)
        } else {
            launchGenericStoredFileSave(requestId, safeName, source, mimeType)
        }
        return true
    }

    private fun launchGenericStoredFileSave(
        requestId: String,
        fileName: String,
        source: File,
        mimeType: String,
    ) {
        handler.post {
            try {
                pendingGenericFileSave = PendingFileSave(requestId, fileName, null, source, mimeType)
                val intent = Intent(Intent.ACTION_CREATE_DOCUMENT).apply {
                    addCategory(Intent.CATEGORY_OPENABLE)
                    type = mimeType.takeIf { it.isNotBlank() } ?: "application/octet-stream"
                    putExtra(Intent.EXTRA_TITLE, fileName)
                }
                genericFileDocumentLauncher.launch(intent)
            } catch (_: ActivityNotFoundException) {
                pendingGenericFileSave = null
                dispatchFileSaveResult(requestId, false, null, "No file picker available on this device")
            } catch (_: Throwable) {
                pendingGenericFileSave = null
                dispatchFileSaveResult(requestId, false, null, "Unable to open save picker")
            }
        }
    }

    private fun saveStoredImageToMediaStore(
        requestId: String,
        fileName: String,
        source: File,
        mimeType: String,
    ) {
        Thread({
            var uri: Uri? = null
            try {
                val collection = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                    MediaStore.Images.Media.getContentUri(MediaStore.VOLUME_EXTERNAL_PRIMARY)
                } else {
                    MediaStore.Images.Media.EXTERNAL_CONTENT_URI
                }
                val values = ContentValues().apply {
                    put(MediaStore.Images.Media.DISPLAY_NAME, fileName)
                    put(MediaStore.Images.Media.MIME_TYPE, mimeType)
                    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                        put(MediaStore.Images.Media.RELATIVE_PATH, "Pictures/Ratspeak")
                        put(MediaStore.Images.Media.IS_PENDING, 1)
                    }
                }
                uri = contentResolver.insert(collection, values)
                    ?: throw IllegalStateException("Could not create image in Photos")
                contentResolver.openOutputStream(uri)?.use { output ->
                    FileInputStream(source).use { input -> input.copyTo(output, 64 * 1024) }
                } ?: throw IllegalStateException("Could not open image destination")
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                    val done = ContentValues().apply { put(MediaStore.Images.Media.IS_PENDING, 0) }
                    contentResolver.update(uri, done, null, null)
                }
                dispatchFileSaveResult(requestId, true, uri.toString(), null)
            } catch (_: Throwable) {
                if (uri != null) try { contentResolver.delete(uri, null, null) } catch (_: Throwable) {}
                dispatchFileSaveResult(requestId, false, null, "Failed to save image")
            }
        }, "ratspeak-photo-stream-save").start()
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
        fun setColorMode(mode: String) {
            applySystemBarColorMode(mode)
        }

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
            val parsed = try { url.trim().toUri() } catch (_: Throwable) { return false }
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
        fun openSupportEmail(subject: String, body: String): Boolean {
            val cleanSubject = subject.trim()
            if (cleanSubject.isEmpty() || cleanSubject.length > 180 || body.length > 8_000) {
                return false
            }
            if (cleanSubject.any { it == '\r' || it == '\n' || it == '\u0000' } || body.contains('\u0000')) {
                return false
            }
            val uri = "mailto:mail@ratspeak.org".toUri().buildUpon()
                .appendQueryParameter("subject", cleanSubject)
                .appendQueryParameter("body", body)
                .build()
            val intent = Intent(Intent.ACTION_SENDTO, uri)
            return try {
                startActivity(intent)
                true
            } catch (_: ActivityNotFoundException) {
                false
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
        fun notificationAuthorizationStatus(): String {
            val status = RatspeakNativeBridge.notificationAuthorizationStatus()
            if (status != "denied" || Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) {
                return status
            }
            val asked = getSharedPreferences("ratspeak_mobile_permissions", Context.MODE_PRIVATE)
                .getBoolean("notifications_requested", false)
            return if (asked) "denied" else "prompt"
        }

        @JavascriptInterface
        fun openNotificationSettings(): Boolean {
            return RatspeakNativeBridge.openNotificationSettings()
        }

        @JavascriptInterface
        fun requestNotificationPermission() {
            if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU ||
                ContextCompat.checkSelfPermission(
                    this@MainActivity,
                    Manifest.permission.POST_NOTIFICATIONS,
                ) == PackageManager.PERMISSION_GRANTED
            ) return
            handler.post {
                getSharedPreferences("ratspeak_mobile_permissions", Context.MODE_PRIVATE)
                    .edit { putBoolean("notifications_requested", true) }
                ActivityCompat.requestPermissions(
                    this@MainActivity,
                    arrayOf(Manifest.permission.POST_NOTIFICATIONS),
                    1002,
                )
            }
        }

        @JavascriptInterface
        fun batteryOptimizationStatus(): String {
            return RatspeakNativeBridge.batteryOptimizationStatus()
        }

        @JavascriptInterface
        fun requestBatteryOptimizationExemption(): Boolean {
            return RatspeakNativeBridge.requestBatteryOptimizationExemption()
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
        fun playCallTimeoutCue(): Boolean {
            return this@MainActivity.runOnMainForBoolean {
                this@MainActivity.startNativeCallRingtone("timeout")
            }
        }

        @JavascriptInterface
        fun primeCallAudioRoute(role: String) {
            handler.post {
                this@MainActivity.primeNativeCallAudioRoute(role)
            }
        }

        @JavascriptInterface
        fun startCallAudioRoute(role: String, sessionToken: String) {
            handler.post {
                this@MainActivity.updateNativeCallAudioRoute(role, sessionToken)
            }
        }

        @JavascriptInterface
        fun stopCallAudioRoute() {
            handler.post {
                this@MainActivity.stopNativeCallAudioRoute()
            }
        }

        @JavascriptInterface
        fun startVoiceMemoAudioSession(): Boolean {
            return this@MainActivity.runOnMainForBoolean {
                this@MainActivity.startNativeVoiceMemoAudioSession()
            }
        }

        @JavascriptInterface
        fun stopVoiceMemoAudioSession() {
            handler.post {
                this@MainActivity.stopNativeVoiceMemoAudioSession()
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

        // ---- USB-OTG permission bridge ----
        //
        // USB permissions are requested only from this visible user action;
        // the process Service owns the non-exported result receiver and OS
        // attach/detach observation. The flow is:
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
            handler.post { RatspeakPlatformSupervisor.requestUsbPermission(deviceName) }
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

    internal fun onNativeBleProgress(token: String, generation: Long, phase: String) {
        if (!BLE_OPERATION_RE.matches(token) || generation < 0) return
        val payload = JSONObject()
            .put("activity_operation", token)
            .put("native_generation", generation.toString())
            .put("phase", phase)
        handler.post {
            webViewRef?.evaluateJavascript(
                "if(typeof window._onBleConnectProgress==='function')window._onBleConnectProgress($payload);",
                null,
            )
        }
    }

    internal fun onNativeUsbPermission(deviceName: String, granted: Boolean, error: String?) {
        dispatchUsbResult(deviceName, granted, error)
    }

    internal fun onNativeUsbSelectorPermission(granted: Boolean, errorCode: String?) {
        val payload = JSONObject().put("granted", granted)
        if (errorCode != null) payload.put("error_code", errorCode)
        handler.post {
            webViewRef?.evaluateJavascript(
                "if(typeof window._onUsbSelectorPermissionResult==='function')window._onUsbSelectorPermissionResult($payload);",
                null,
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
