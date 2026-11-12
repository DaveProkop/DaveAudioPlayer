//! Android glue: hands the JavaVM and the application context to `ndk-context`, which
//! cpal's AAudio backend needs to query audio devices. Called from `MainActivity` before
//! Flutter (and with it the engine) starts.

use std::sync::Once;

use jni_sys::{jobject, JNIEnv, JavaVM, JNI_OK};

static INIT: Once = Once::new();

/// `MainActivity.initNative(context)`.
///
/// # Safety
/// Called by the JVM with a valid `env` and a live `context` object.
#[no_mangle]
pub unsafe extern "system" fn Java_cz_daveprokop_daveaudioplayer_MainActivity_initNative(
    env: *mut JNIEnv,
    _this: jobject,
    context: jobject,
) {
    INIT.call_once(|| {
        // SAFETY: env and context come straight from the JVM for this call.
        unsafe {
            // jni-sys 0.4 groups the function table by JNI version; both calls exist since 1.1.
            let fns = &(**env).v1_1;
            let mut vm: *mut JavaVM = std::ptr::null_mut();
            if (fns.GetJavaVM)(env, &mut vm) != JNI_OK || vm.is_null() {
                return;
            }
            // A global reference: the context must outlive this call.
            let global = (fns.NewGlobalRef)(env, context);
            if global.is_null() {
                return;
            }
            ndk_context::initialize_android_context(vm.cast(), global.cast());
        }
    });
}
