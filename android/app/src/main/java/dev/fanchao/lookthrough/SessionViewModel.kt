package dev.fanchao.lookthrough

import android.app.Application
import android.content.Context
import androidx.core.content.edit
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import dev.fanchao.lookthrough.ffi.Session
import dev.fanchao.lookthrough.ffi.SessionListener
import dev.fanchao.lookthrough.ffi.SessionOptions
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

sealed interface UiState {
    /** Not connected; [message] says why the last session ended, if it did. */
    data class Idle(val message: String? = null) : UiState

    data object Connecting : UiState

    data class Connected(val session: Session, val name: String) : UiState
}

data class ConnectSettings(val address: String, val quality: Int?, val resize: Boolean)

/** Holds the session across configuration changes. */
class SessionViewModel(app: Application) : AndroidViewModel(app) {
    private val prefs = app.getSharedPreferences("lookthrough", Context.MODE_PRIVATE)
    private val _state = MutableStateFlow<UiState>(UiState.Idle())
    val state: StateFlow<UiState> = _state.asStateFlow()

    /** Bumped per connection, so callbacks from an old session are ignored. */
    private var generation = 0

    val settings: ConnectSettings
        get() =
            ConnectSettings(
                address = prefs.getString("address", null) ?: "10.0.2.2:5901",
                quality = prefs.getInt("quality", 7).takeIf { it >= 0 },
                resize = prefs.getBoolean("resize", true),
            )

    fun connect(s: ConnectSettings) {
        if (_state.value !is UiState.Idle) return
        prefs.edit {
            putString("address", s.address)
            putInt("quality", s.quality ?: -1)
            putBoolean("resize", s.resize)
        }
        val gen = ++generation
        _state.value = UiState.Connecting
        viewModelScope.launch {
            val result =
                withContext(Dispatchers.IO) {
                    runCatching {
                        Session.connect(
                            s.address,
                            SessionOptions(s.quality?.toUByte(), s.resize),
                            Listener(gen),
                        )
                    }
                }
            if (gen != generation) {
                result.getOrNull()?.let(::release)
                return@launch
            }
            _state.value =
                result.fold(
                    { UiState.Connected(it, s.address) },
                    { UiState.Idle("Couldn't connect to ${s.address}: ${it.message}") },
                )
        }
    }

    fun disconnect(message: String? = null) {
        generation++
        val current = _state.value
        _state.value = UiState.Idle(message)
        if (current is UiState.Connected) release(current.session)
    }

    /** Stops the session's threads and frees it, off the main thread. */
    private fun release(session: Session) {
        Thread {
            session.disconnect()
            session.close()
        }
            .start()
    }

    override fun onCleared() {
        disconnect()
    }

    /** Callbacks arrive on Rust threads; hop to the main thread. */
    private inner class Listener(private val gen: Int) : SessionListener {
        override fun onDesktopName(name: String) {
            viewModelScope.launch(Dispatchers.Main) {
                val s = _state.value
                if (gen == generation && s is UiState.Connected) _state.value = s.copy(name = name)
            }
        }

        override fun onClosed(error: String?) {
            viewModelScope.launch(Dispatchers.Main) {
                if (gen == generation && _state.value is UiState.Connected) {
                    disconnect(error?.let { "Session ended: $it" } ?: "The server closed the session.")
                }
            }
        }
    }
}
