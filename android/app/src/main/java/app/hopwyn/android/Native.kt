package app.hopwyn.android

/** The Rust core (crates/hopwyn-mobile), loaded from libhopwyn_mobile.so. */
object Native {
    init {
        System.loadLibrary("hopwyn_mobile")
    }

    /** Starts the core once. Returns an error message, or null. */
    external fun nativeStart(dir: String, name: String): String?

    /** The next event line, waiting up to [timeoutMs], or null. */
    external fun nativeNextEvent(timeoutMs: Int): String?

    external fun nativeAddComputer()
    external fun nativeCancelPairing()
    external fun nativeConfirm(yes: Boolean)
    external fun nativeJoin(id: String)

    /** Joins the pairing window of the computer at this typed address. */
    external fun nativeJoinAddress(address: String)

    /** Where to look for a paired computer that isn't found on its own. */
    external fun nativeSetAddress(id: String, address: String)

    /** The phone joined, left or changed network. */
    external fun nativeNetworkChanged()
    external fun nativeControl(id: String)
    external fun nativeRelease()
    external fun nativeForget(id: String)
    external fun nativeMotion(dx: Float, dy: Float)

    /** [button]: 0 left, 1 right, 2 middle. */
    external fun nativeButton(button: Int, pressed: Boolean)
    external fun nativeScroll(dx: Float, dy: Float)

    /** One USB HID key (page 0x07). */
    external fun nativeKey(code: Int, pressed: Boolean)
    external fun nativeText(text: String)

    /** Whether computers may drive this phone, and its screen size. */
    external fun nativeSetControllable(enabled: Boolean, width: Int, height: Int)

    /** The next on-screen action while a computer drives the phone, or null. */
    external fun nativeNextControl(timeoutMs: Int): String?
}

/** USB HID usages for the extra keys on the keyboard bar. */
object Hid {
    const val ENTER = 0x28
    const val ESCAPE = 0x29
    const val BACKSPACE = 0x2a
    const val TAB = 0x2b
    const val RIGHT = 0x4f
    const val LEFT = 0x50
    const val DOWN = 0x51
    const val UP = 0x52
    const val CTRL = 0xe0
    const val ALT = 0xe2
    const val SUPER = 0xe3
}
