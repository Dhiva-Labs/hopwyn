package app.hopwyn.android

import android.content.Context
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue

enum class ThemeMode(val label: String) {
    System("System default"),
    Light("Light"),
    Dark("Dark"),
}

/** The user's choices, saved on the phone and observable by Compose. */
class Prefs(context: Context) {
    private val store = context.applicationContext.getSharedPreferences("settings", Context.MODE_PRIVATE)

    var theme by mutableStateOf(
        runCatching { ThemeMode.valueOf(store.getString(THEME, null) ?: "") }.getOrDefault(ThemeMode.System),
    )
        private set
    var wallpaperColors by mutableStateOf(store.getBoolean(WALLPAPER, false))
        private set
    /** Computer pixels per finger pixel. */
    var pointerSpeed by mutableFloatStateOf(store.getFloat(SPEED, DEFAULT_SPEED))
        private set
    /** Content follows the fingers, like scrolling the phone itself. */
    var naturalScroll by mutableStateOf(store.getBoolean(NATURAL, true))
        private set
    /** Whether the user has read and agreed to what phone control can do. */
    var controlDisclosed by mutableStateOf(store.getBoolean(DISCLOSED, false))
        private set

    fun updateTheme(mode: ThemeMode) {
        theme = mode
        store.edit().putString(THEME, mode.name).apply()
    }

    fun updateWallpaperColors(on: Boolean) {
        wallpaperColors = on
        store.edit().putBoolean(WALLPAPER, on).apply()
    }

    fun updatePointerSpeed(speed: Float) {
        pointerSpeed = speed
        store.edit().putFloat(SPEED, speed).apply()
    }

    fun updateNaturalScroll(on: Boolean) {
        naturalScroll = on
        store.edit().putBoolean(NATURAL, on).apply()
    }

    fun agreeToControl() {
        controlDisclosed = true
        store.edit().putBoolean(DISCLOSED, true).apply()
    }

    companion object {
        const val DEFAULT_SPEED = 1.6f
        private const val THEME = "theme"
        private const val WALLPAPER = "wallpaper_colors"
        private const val SPEED = "pointer_speed"
        private const val NATURAL = "natural_scroll"
        private const val DISCLOSED = "control_disclosed"

        @Volatile private var instance: Prefs? = null

        fun get(context: Context): Prefs =
            instance ?: synchronized(this) { instance ?: Prefs(context).also { instance = it } }
    }
}
