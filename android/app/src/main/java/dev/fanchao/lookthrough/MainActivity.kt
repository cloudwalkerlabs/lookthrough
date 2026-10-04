package dev.fanchao.lookthrough

import android.os.Build
import android.os.Bundle
import android.system.Os
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.setContent
import androidx.activity.viewModels
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Slider
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.fanchao.lookthrough.ffi.initLogging

class MainActivity : ComponentActivity() {
    private val vm: SessionViewModel by viewModels()

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        if (Build.HARDWARE == "ranchu") {
            // The emulator's gfxstream Vulkan driver crashes in
            // vkQueueSubmit; its GLES works.
            Os.setenv("WGPU_BACKEND", "gl", false)
        }
        initLogging("info,wgpu_core=warn,wgpu_hal=error,naga=warn")
        setContent {
            MaterialTheme(colorScheme = darkColorScheme()) {
                Surface(Modifier.fillMaxSize()) { App(vm) }
            }
        }
    }
}

@Composable
private fun App(vm: SessionViewModel) {
    val state by vm.state.collectAsStateWithLifecycle()
    when (val s = state) {
        is UiState.Connected -> {
            BackHandler { vm.disconnect() }
            // A new view per session, so its surface binds to that session.
            val id = s.session.id().toLong()
            key(id) {
                AndroidView(
                    factory = { SessionView(it, id) },
                    modifier = Modifier.fillMaxSize(),
                )
            }
        }
        else ->
            ConnectScreen(
                initial = vm.settings,
                connecting = s is UiState.Connecting,
                message = (s as? UiState.Idle)?.message,
                onConnect = vm::connect,
            )
    }
}

@Composable
private fun ConnectScreen(
    initial: ConnectSettings,
    connecting: Boolean,
    message: String?,
    onConnect: (ConnectSettings) -> Unit,
) {
    var address by remember { mutableStateOf(initial.address) }
    // 10 means lossless; 0-9 are JPEG qualities.
    var quality by remember { mutableFloatStateOf((initial.quality ?: 10).toFloat()) }
    var resize by remember { mutableStateOf(initial.resize) }
    Box(Modifier.fillMaxSize().safeDrawingPadding(), contentAlignment = Alignment.Center) {
        Column(
            Modifier.widthIn(max = 420.dp).padding(24.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            Text("lookthrough", style = MaterialTheme.typography.headlineMedium)
            OutlinedTextField(
                value = address,
                onValueChange = { address = it },
                label = { Text("Server (host:port)") },
                singleLine = true,
                enabled = !connecting,
            )
            val q = quality.toInt()
            Text(if (q == 10) "Quality: lossless" else "JPEG quality: $q")
            Slider(
                value = quality,
                onValueChange = { quality = it },
                valueRange = 0f..10f,
                steps = 9,
                enabled = !connecting,
            )
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text("Resize server to fit", Modifier.weight(1f))
                Switch(checked = resize, onCheckedChange = { resize = it }, enabled = !connecting)
            }
            if (connecting) {
                CircularProgressIndicator()
            } else {
                Button(
                    onClick = {
                        onConnect(ConnectSettings(address.trim(), q.takeIf { it < 10 }, resize))
                    },
                    enabled = address.isNotBlank(),
                ) {
                    Text("Connect")
                }
            }
            message?.let { Text(it, color = Color(0xFFFFB4AB)) }
        }
    }
}
