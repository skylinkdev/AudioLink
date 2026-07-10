package dev.local.lan_audio_flutter

import android.content.Context
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import android.net.wifi.WifiManager
import android.os.Build
import java.net.InetAddress
import java.util.Collections
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

class MdnsDiscovery(private val context: Context) {
    companion object {
        private const val SERVICE_TYPE = "_lan-audio._tcp."
        private const val SCAN_TIMEOUT_MS = 1800L
    }

    fun scan(): List<Map<String, Any?>> {
        val wifiManager = context.applicationContext.getSystemService(Context.WIFI_SERVICE) as? WifiManager
        val multicastLock = wifiManager?.createMulticastLock("lan-audio-mdns")?.apply {
            setReferenceCounted(false)
            acquire()
        }

        return try {
            scanWithNsd()
        } finally {
            multicastLock?.let {
                if (it.isHeld) it.release()
            }
        }
    }

    private fun scanWithNsd(): List<Map<String, Any?>> {
        val nsdManager = context.getSystemService(Context.NSD_SERVICE) as NsdManager
        val latch = CountDownLatch(1)
        val discovered = Collections.synchronizedMap(linkedMapOf<String, Map<String, Any?>>())

        val listener = object : NsdManager.DiscoveryListener {
            override fun onDiscoveryStarted(serviceType: String) = Unit

            override fun onServiceFound(serviceInfo: NsdServiceInfo) {
                if (serviceInfo.serviceType != SERVICE_TYPE) return

                nsdManager.resolveService(serviceInfo, object : NsdManager.ResolveListener {
                    override fun onResolveFailed(serviceInfo: NsdServiceInfo, errorCode: Int) = Unit

                    override fun onServiceResolved(resolved: NsdServiceInfo) {
                        val host = resolved.hostAddress() ?: return
                        val key = "${host.hostAddress}:${resolved.port}"
                        discovered[key] = mapOf(
                            "name" to resolved.serviceName,
                            "host" to host.hostAddress,
                            "port" to resolved.port,
                            "sampleRate" to resolved.txtValue("sample_rate"),
                            "channels" to resolved.txtValue("channels"),
                            "frameMs" to resolved.txtValue("frame_ms"),
                            "format" to resolved.txtValue("format"),
                            "controlPort" to (
                                resolved.txtValue("control_port")?.toIntOrNull() ?: resolved.port
                            ),
                        )
                    }
                })
            }

            override fun onServiceLost(serviceInfo: NsdServiceInfo) = Unit

            override fun onDiscoveryStopped(serviceType: String) {
                latch.countDown()
            }

            override fun onStartDiscoveryFailed(serviceType: String, errorCode: Int) {
                latch.countDown()
            }

            override fun onStopDiscoveryFailed(serviceType: String, errorCode: Int) {
                latch.countDown()
            }
        }

        nsdManager.discoverServices(SERVICE_TYPE, NsdManager.PROTOCOL_DNS_SD, listener)
        Thread.sleep(SCAN_TIMEOUT_MS)
        runCatching { nsdManager.stopServiceDiscovery(listener) }
        latch.await(500, TimeUnit.MILLISECONDS)

        return discovered.values.toList()
    }

    private fun NsdServiceInfo.hostAddress(): InetAddress? {
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            hostAddresses.firstOrNull()
        } else {
            @Suppress("DEPRECATION")
            host
        }
    }

    private fun NsdServiceInfo.txtValue(key: String): String? {
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.LOLLIPOP) {
            attributes[key]?.toString(Charsets.UTF_8)
        } else {
            null
        }
    }
}
