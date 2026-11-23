//! The JNI bridge for `app.hopwyn.android.Native` (Kotlin `external fun`s).
//!
//! One core per process. Every entry point is a thin translation to a
//! [`Command`]; nothing here can panic across the JNI boundary.
#![allow(unsafe_code)] // #[unsafe(no_mangle)] on the JNI symbols only.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use hopwyn_proto::Button;
use jni::JNIEnv;
use jni::objects::{JClass, JObject, JString};
use jni::sys::{jboolean, jfloat, jint, jstring};

use crate::{Command, Mobile, Ports, parse_id};

/// The core, once started. Read without a lock, so waiting for events
/// never holds up input.
static CORE: OnceLock<Mobile> = OnceLock::new();
/// Serialises starting.
static STARTING: Mutex<()> = Mutex::new(());

fn send(cmd: Command) {
    if let Some(m) = CORE.get() {
        m.send(cmd);
    }
}

fn string(env: &mut JNIEnv<'_>, s: &JString<'_>) -> String {
    env.get_string(s).map(Into::into).unwrap_or_default()
}

fn to_java(env: &mut JNIEnv<'_>, s: Option<String>) -> jstring {
    match s.and_then(|s| env.new_string(s).ok()) {
        Some(js) => js.into_raw(),
        None => JObject::null().into_raw(),
    }
}

/// Starts the core (once). Returns an error message, or null on success.
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_hopwyn_android_Native_nativeStart<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    dir: JString<'l>,
    name: JString<'l>,
) -> jstring {
    let (dir, name) = (string(&mut env, &dir), string(&mut env, &name));
    let _guard = STARTING.lock();
    let result = if CORE.get().is_some() {
        None
    } else {
        match Mobile::start(PathBuf::from(dir), &name, Ports::default()) {
            Ok(m) => {
                let _ = CORE.set(m);
                None
            }
            Err(e) => Some(e),
        }
    };
    to_java(&mut env, result)
}

/// The next event line, waiting up to `timeout_ms`, or null.
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_hopwyn_android_Native_nativeNextEvent<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    timeout_ms: jint,
) -> jstring {
    let wait = Duration::from_millis(u64::try_from(timeout_ms).unwrap_or(0));
    let line = CORE
        .get()
        .and_then(|m| m.next_event(wait))
        .map(|e| e.to_line());
    to_java(&mut env, line)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_hopwyn_android_Native_nativeAddComputer<'l>(
    _env: JNIEnv<'l>,
    _class: JClass<'l>,
) {
    send(Command::AddComputer);
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_hopwyn_android_Native_nativeCancelPairing<'l>(
    _env: JNIEnv<'l>,
    _class: JClass<'l>,
) {
    send(Command::CancelPairing);
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_hopwyn_android_Native_nativeConfirm<'l>(
    _env: JNIEnv<'l>,
    _class: JClass<'l>,
    yes: jboolean,
) {
    send(Command::Confirm(yes != 0));
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_hopwyn_android_Native_nativeJoinAddress<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    address: JString<'l>,
) {
    send(Command::JoinAddress(string(&mut env, &address)));
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_hopwyn_android_Native_nativeSetAddress<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    id: JString<'l>,
    address: JString<'l>,
) {
    let address = string(&mut env, &address);
    with_id(&mut env, &id, |id| Command::SetAddress(id, address));
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_hopwyn_android_Native_nativeNetworkChanged<'l>(
    _env: JNIEnv<'l>,
    _class: JClass<'l>,
) {
    send(Command::NetworkChanged);
}

fn with_id(
    env: &mut JNIEnv<'_>,
    id: &JString<'_>,
    make: impl FnOnce(hopwyn_pair::DeviceId) -> Command,
) {
    if let Some(id) = parse_id(&string(env, id)) {
        send(make(id));
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_hopwyn_android_Native_nativeJoin<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    id: JString<'l>,
) {
    with_id(&mut env, &id, Command::Join);
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_hopwyn_android_Native_nativeControl<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    id: JString<'l>,
) {
    with_id(&mut env, &id, Command::Control);
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_hopwyn_android_Native_nativeForget<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    id: JString<'l>,
) {
    with_id(&mut env, &id, Command::Forget);
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_hopwyn_android_Native_nativeRelease<'l>(
    _env: JNIEnv<'l>,
    _class: JClass<'l>,
) {
    send(Command::Release);
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_hopwyn_android_Native_nativeMotion<'l>(
    _env: JNIEnv<'l>,
    _class: JClass<'l>,
    dx: jfloat,
    dy: jfloat,
) {
    send(Command::Motion(dx, dy));
}

/// `button`: 0 left, 1 right, 2 middle.
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_hopwyn_android_Native_nativeButton<'l>(
    _env: JNIEnv<'l>,
    _class: JClass<'l>,
    button: jint,
    pressed: jboolean,
) {
    let button = match button {
        1 => Button::Right,
        2 => Button::Middle,
        _ => Button::Left,
    };
    send(Command::Button(button, pressed != 0));
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_hopwyn_android_Native_nativeScroll<'l>(
    _env: JNIEnv<'l>,
    _class: JClass<'l>,
    dx: jfloat,
    dy: jfloat,
) {
    send(Command::Scroll(dx, dy));
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_hopwyn_android_Native_nativeKey<'l>(
    _env: JNIEnv<'l>,
    _class: JClass<'l>,
    code: jint,
    pressed: jboolean,
) {
    if let Ok(code) = u16::try_from(code) {
        send(Command::Key(code, pressed != 0));
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_hopwyn_android_Native_nativeText<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    text: JString<'l>,
) {
    let text = string(&mut env, &text);
    send(Command::Text(text));
}

/// Whether computers may drive this phone (the Accessibility service is
/// on), with the screen size in pixels.
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_hopwyn_android_Native_nativeSetControllable<'l>(
    _env: JNIEnv<'l>,
    _class: JClass<'l>,
    enabled: jboolean,
    width: jint,
    height: jint,
) {
    send(Command::SetControllable {
        enabled: enabled != 0,
        width: u32::try_from(width).unwrap_or(0),
        height: u32::try_from(height).unwrap_or(0),
    });
}

/// The next on-screen action while a computer drives the phone, waiting up
/// to `timeout_ms`, or null.
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_hopwyn_android_Native_nativeNextControl<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    timeout_ms: jint,
) -> jstring {
    let wait = Duration::from_millis(u64::try_from(timeout_ms).unwrap_or(0));
    let line = CORE
        .get()
        .and_then(|m| m.next_control(wait))
        .map(|c| c.to_line());
    to_java(&mut env, line)
}
