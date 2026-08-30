//! Local shared-service keys. Never fall back to settings or a plaintext file.

use zeroize::Zeroizing;

const STORAGE_ERROR: &str = "Protected credential storage is unavailable or locked. Unlock your system keychain and try again. No key was saved in plaintext.";

pub(crate) trait CredentialStore: Send + Sync {
    fn read(&self, id: &str) -> Result<Zeroizing<Vec<u8>>, String>;
    fn write(&self, id: &str, key: &[u8]) -> Result<(), String>;
    fn delete(&self, id: &str) -> Result<(), String>;
}

pub(crate) struct NativeCredentialStore;
impl CredentialStore for NativeCredentialStore {
    fn read(&self, id: &str) -> Result<Zeroizing<Vec<u8>>, String> {
        read(id)
    }
    fn write(&self, id: &str, key: &[u8]) -> Result<(), String> {
        write(id, key)
    }
    fn delete(&self, id: &str) -> Result<(), String> {
        delete(id)
    }
}

pub(crate) fn read(id: &str) -> Result<Zeroizing<Vec<u8>>, String> {
    platform::read(id)
        .map(Zeroizing::new)
        .map_err(|_| STORAGE_ERROR.to_string())
}

pub(crate) fn write(id: &str, key: &[u8]) -> Result<(), String> {
    platform::write(id, key).map_err(|_| STORAGE_ERROR.to_string())
}

pub(crate) fn delete(id: &str) -> Result<(), String> {
    platform::delete(id).map_err(|_| STORAGE_ERROR.to_string())
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "windows",
    target_os = "linux"
))]
mod platform {
    fn entry(id: &str) -> Result<keyring::Entry, ()> {
        keyring::Entry::new("org.ratspeak.shared-instance.v1", id).map_err(|_| ())
    }
    pub fn read(id: &str) -> Result<Vec<u8>, ()> {
        entry(id)?.get_secret().map_err(|_| ())
    }
    pub fn write(id: &str, key: &[u8]) -> Result<(), ()> {
        entry(id)?.set_secret(key).map_err(|_| ())
    }
    pub fn delete(id: &str) -> Result<(), ()> {
        match entry(id)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err(()),
        }
    }
}

#[cfg(target_os = "android")]
mod platform {
    use jni::objects::{JClass, JObject, JValue};

    fn with_store<T>(
        f: impl FnOnce(&jni::JNIEnv, JClass, JObject) -> jni::errors::Result<T>,
    ) -> Result<T, ()> {
        let vm = rns_interface::android_usb::java_vm().ok_or(())?;
        let env = vm.attach_current_thread().map_err(|_| ())?;
        let result = (|| {
            let thread = env.find_class("android/app/ActivityThread")?;
            let app = env
                .call_static_method(
                    thread,
                    "currentApplication",
                    "()Landroid/app/Application;",
                    &[],
                )?
                .l()?;
            let loader = env
                .call_method(app, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])?
                .l()?;
            let name = env.new_string("org.ratspeak.android.RatspeakSharedSecrets")?;
            let class = env
                .call_method(
                    loader,
                    "loadClass",
                    "(Ljava/lang/String;)Ljava/lang/Class;",
                    &[JValue::Object(name.into())],
                )?
                .l()?;
            f(&env, JClass::from(class), app)
        })();
        if env.exception_check().unwrap_or(false) {
            let _ = env.exception_clear();
        }
        result.map_err(|_| ())
    }
    pub fn read(id: &str) -> Result<Vec<u8>, ()> {
        with_store(|env, class, app| {
            let id = env.new_string(id)?;
            let array = env
                .call_static_method(
                    class,
                    "read",
                    "(Landroid/content/Context;Ljava/lang/String;)[B",
                    &[JValue::Object(app), JValue::Object(id.into())],
                )?
                .l()?;
            let array = array.into_inner();
            let result = env.convert_byte_array(array);
            let len = env.get_array_length(array)?;
            env.set_byte_array_region(array, 0, &vec![0; len as usize])?;
            result
        })
    }
    pub fn write(id: &str, key: &[u8]) -> Result<(), ()> {
        with_store(|env, class, app| {
            let id = env.new_string(id)?;
            let bytes = env.byte_array_from_slice(key)?;
            env.call_static_method(
                class,
                "write",
                "(Landroid/content/Context;Ljava/lang/String;[B)V",
                &[
                    JValue::Object(app),
                    JValue::Object(id.into()),
                    JValue::Object(JObject::from(bytes)),
                ],
            )?;
            Ok(())
        })
    }
    pub fn delete(id: &str) -> Result<(), ()> {
        with_store(|env, class, app| {
            let id = env.new_string(id)?;
            env.call_static_method(
                class,
                "delete",
                "(Landroid/content/Context;Ljava/lang/String;)V",
                &[JValue::Object(app), JValue::Object(id.into())],
            )?;
            Ok(())
        })
    }
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "windows",
    target_os = "linux",
    target_os = "android"
)))]
mod platform {
    pub fn read(_: &str) -> Result<Vec<u8>, ()> {
        Err(())
    }
    pub fn write(_: &str, _: &[u8]) -> Result<(), ()> {
        Err(())
    }
    pub fn delete(_: &str) -> Result<(), ()> {
        Err(())
    }
}
