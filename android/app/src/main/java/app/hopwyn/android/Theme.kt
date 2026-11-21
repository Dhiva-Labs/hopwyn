package app.hopwyn.android

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Typography
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.sp

/** Hopwyn's own palette: cool slate, the logo's blue, mint for "connected". */
private object Palette {
    val Blue = Color(0xFF3B78F0)
    val BlueSoft = Color(0xFF8DB0FF)
    val Mint = Color(0xFF239A6C)
    val MintSoft = Color(0xFF5FD3A2)

    val Paper = Color(0xFFEEF1F5)
    val PaperRaised = Color(0xFFFFFFFF)
    val Ink = Color(0xFF18202E)
    val InkMuted = Color(0xFF5E6878)
    val Line = Color(0xFFD5DBE4)

    val Night = Color(0xFF121722)
    val NightRaised = Color(0xFF1B2230)
    val Snow = Color(0xFFE5E9F1)
    val SnowMuted = Color(0xFF97A1B2)
    val NightLine = Color(0xFF2C3546)
}

/** Colours Material's scheme has no slot for. */
@Immutable
data class HopwynColors(
    val online: Color,
    val line: Color,
    val muted: Color,
)

val LocalHopwynColors = staticCompositionLocalOf {
    HopwynColors(Palette.Mint, Palette.Line, Palette.InkMuted)
}

private val Light = lightColorScheme(
    primary = Palette.Blue,
    onPrimary = Color.White,
    primaryContainer = Color(0xFFDCE6FD),
    onPrimaryContainer = Color(0xFF0D2C6B),
    secondaryContainer = Color(0xFFDCE6FD),
    onSecondaryContainer = Color(0xFF0D2C6B),
    background = Palette.Paper,
    onBackground = Palette.Ink,
    surface = Palette.Paper,
    onSurface = Palette.Ink,
    surfaceVariant = Palette.PaperRaised,
    onSurfaceVariant = Palette.InkMuted,
    surfaceContainer = Palette.PaperRaised,
    surfaceContainerHigh = Palette.PaperRaised,
    surfaceContainerHighest = Palette.PaperRaised,
    outline = Palette.Line,
    outlineVariant = Palette.Line,
)

private val Dark = darkColorScheme(
    primary = Palette.BlueSoft,
    onPrimary = Color(0xFF0D2C6B),
    primaryContainer = Color(0xFF233A6B),
    onPrimaryContainer = Color(0xFFDCE6FD),
    secondaryContainer = Color(0xFF233A6B),
    onSecondaryContainer = Color(0xFFDCE6FD),
    background = Palette.Night,
    onBackground = Palette.Snow,
    surface = Palette.Night,
    onSurface = Palette.Snow,
    surfaceVariant = Palette.NightRaised,
    onSurfaceVariant = Palette.SnowMuted,
    surfaceContainer = Palette.NightRaised,
    surfaceContainerHigh = Palette.NightRaised,
    surfaceContainerHighest = Palette.NightRaised,
    outline = Palette.NightLine,
    outlineVariant = Palette.NightLine,
)

private val Type = Typography().run {
    copy(
        headlineMedium = headlineMedium.copy(fontWeight = FontWeight.Bold, letterSpacing = (-0.5).sp),
        titleLarge = titleLarge.copy(fontWeight = FontWeight.SemiBold, letterSpacing = (-0.2).sp),
        titleMedium = titleMedium.copy(fontWeight = FontWeight.SemiBold),
        titleSmall = titleSmall.copy(fontWeight = FontWeight.SemiBold, fontSize = 15.sp),
        bodyMedium = bodyMedium.copy(lineHeight = 21.sp),
    )
}

/** The pairing code: big, even digits that are easy to compare across a desk. */
val CodeStyle = TextStyle(
    fontSize = 44.sp,
    fontWeight = FontWeight.Bold,
    letterSpacing = 6.sp,
    fontFeatureSettings = "tnum",
)

@Composable
fun HopwynTheme(prefs: Prefs, content: @Composable () -> Unit) {
    val dark = when (prefs.theme) {
        ThemeMode.System -> isSystemInDarkTheme()
        ThemeMode.Light -> false
        ThemeMode.Dark -> true
    }
    val context = androidx.compose.ui.platform.LocalContext.current
    val scheme = when {
        prefs.wallpaperColors && android.os.Build.VERSION.SDK_INT >= 31 ->
            if (dark) androidx.compose.material3.dynamicDarkColorScheme(context)
            else androidx.compose.material3.dynamicLightColorScheme(context)
        dark -> Dark
        else -> Light
    }
    val extra = if (dark) {
        HopwynColors(Palette.MintSoft, Palette.NightLine, Palette.SnowMuted)
    } else {
        HopwynColors(Palette.Mint, Palette.Line, Palette.InkMuted)
    }
    androidx.compose.runtime.CompositionLocalProvider(LocalHopwynColors provides extra) {
        MaterialTheme(colorScheme = scheme, typography = Type, content = content)
    }
}
