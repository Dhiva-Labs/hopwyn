# The Rust core's JNI functions are bound by class and method name
# (Java_app_hopwyn_android_Native_*), so Native must keep both.
-keep class app.hopwyn.android.Native { *; }
