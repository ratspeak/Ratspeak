// Copyright 2020-2023 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

use http::{
  header::{HeaderName, HeaderValue, CONTENT_LENGTH, CONTENT_TYPE},
  Request,
};
use jni::errors::Result as JniResult;
pub use jni::{
  self,
  objects::{GlobalRef, JClass, JMap, JObject, JString},
  sys::{jboolean, jint, jobject, jstring},
  JNIEnv,
};
pub use ndk;
use ndk::looper::{FdEvent, ThreadLooper};
use std::os::fd::{AsFd, AsRawFd};

use super::{
  callbacks,
  main_pipe::{MainPipe, MAIN_PIPE},
  EVAL_CALLBACKS,
};

use crate::PageLoadEvent;

#[macro_export]
macro_rules! android_binding {
  ($domain:ident, $package:ident) => {
    ::wry::android_binding!($domain, $package, ::wry)
  };
  // use imported `android_setup` just to force the import path to use `wry::{}`
  // as the macro breaks without braces
  ($domain:ident, $package:ident, $wry:path) => {{
    use $wry::{android_setup as _, prelude::*};

    android_fn!($domain, $package, Rust, wryCreate, []);
    android_fn!(
      $domain,
      $package,
      Rust,
      onWebviewDestroy,
      [JObject, JString]
    );

    android_fn!(
      $domain,
      $package,
      Rust,
      handleRequest,
      [JString, JObject, jboolean],
      jobject
    );
    android_fn!(
      $domain,
      $package,
      Rust,
      withAssetLoader,
      [JString],
      jboolean
    );
    android_fn!(
      $domain,
      $package,
      Rust,
      assetLoaderDomain,
      [JString],
      jstring
    );
    android_fn!(
      $domain,
      $package,
      Rust,
      shouldOverride,
      [JString, JString],
      jboolean
    );
    android_fn!($domain, $package, Rust, onEval, [JString, jint, JString]);
    android_fn!($domain, $package, Rust, onPageLoading, [JString, JString]);
    android_fn!($domain, $package, Rust, onPageLoaded, [JString, JString]);
    android_fn!($domain, $package, Rust, ipc, [JString, JString, JString]);
    android_fn!(
      $domain,
      $package,
      Rust,
      handleReceivedTitle,
      [JString, JString],
    );
  }};
}

fn handle_request(
  env: &mut JNIEnv,
  webview_id: JString,
  request: JObject,
  is_document_start_script_enabled: jboolean,
) -> JniResult<jobject> {
  let webview_id = env.get_string(&webview_id)?;
  let webview_id = webview_id.to_str().ok().unwrap_or_default();

  let Some(callbacks) = callbacks::resolve(webview_id) else {
    return Ok(JObject::null().as_raw());
  };
  if let Some(handler) = &callbacks.request {
    let handler = handler.lock().unwrap();
    // A prior request can hold this per-handler lock until its response arrives.
    // Do not start queued work for a view retired while we were waiting.
    if !callbacks::is_current(webview_id, &callbacks) {
      return Ok(JObject::null().as_raw());
    }
    #[cfg(feature = "tracing")]
    let span =
      tracing::info_span!(parent: None, "wry::custom_protocol::handle", uri = tracing::field::Empty).entered();

    let mut request_builder = Request::builder();

    let uri = env
      .call_method(&request, "getUrl", "()Landroid/net/Uri;", &[])?
      .l()?;
    let url: JString = env
      .call_method(&uri, "toString", "()Ljava/lang/String;", &[])?
      .l()?
      .into();
    let url = env.get_string(&url)?.to_string_lossy().to_string();

    #[cfg(feature = "tracing")]
    span.record("uri", &url);

    request_builder = request_builder.uri(&url);

    let method = env
      .call_method(&request, "getMethod", "()Ljava/lang/String;", &[])?
      .l()
      .map(JString::from)?;
    request_builder = request_builder.method(
      env
        .get_string(&method)?
        .to_string_lossy()
        .to_string()
        .as_str(),
    );

    let request_headers = env
      .call_method(request, "getRequestHeaders", "()Ljava/util/Map;", &[])?
      .l()?;
    let request_headers = JMap::from_env(env, &request_headers)?;
    let mut iter = request_headers.iter(env)?;
    while let Some((header, value)) = iter.next(env)? {
      let header = JString::from(header);
      let value = JString::from(value);
      let header = env.get_string(&header)?;
      let value = env.get_string(&value)?;
      if let (Ok(header), Ok(value)) = (
        HeaderName::from_bytes(header.to_bytes()),
        HeaderValue::from_bytes(value.to_bytes()),
      ) {
        request_builder = request_builder.header(header, value);
      }
    }

    let final_request = match request_builder.body(Vec::new()) {
      Ok(req) => req,
      Err(_e) => {
        #[cfg(feature = "tracing")]
        tracing::warn!("Failed to build response: {_e}");
        return Ok(*JObject::null());
      }
    };

    let response = {
      #[cfg(feature = "tracing")]
      let _span = tracing::info_span!("wry::custom_protocol::call_handler").entered();
      (handler.handler)(
        &callbacks.logical_id,
        final_request,
        is_document_start_script_enabled != 0,
      )
    };
    if let Some(response) = response {
      let status = response.status();
      let status_code = status.as_u16() as i32;
      let status_err = if status_code < 100 {
        Some("Status code can't be less than 100")
      } else if status_code > 599 {
        Some("statusCode can't be greater than 599.")
      } else if status_code > 299 && status_code < 400 {
        Some("statusCode can't be in the [300, 399] range.")
      } else {
        None
      };
      if let Some(_err) = status_err {
        #[cfg(feature = "tracing")]
        tracing::warn!("{_err}");
        return Ok(*JObject::null());
      }

      let reason_phrase = status.canonical_reason().unwrap_or("OK");
      let (mime_type, encoding) = if let Some(content_type) = response.headers().get(CONTENT_TYPE) {
        let content_type = content_type.to_str().unwrap().trim();
        let mut s = content_type.split(';');
        let mime_type = s.next().unwrap().trim();
        let mut encoding = None;
        for token in s {
          let token = token.trim();
          if token.starts_with("charset=") {
            encoding.replace(token.split('=').nth(1).unwrap());
            break;
          }
        }
        (
          env.new_string(mime_type)?,
          if let Some(encoding) = encoding {
            env.new_string(encoding)?
          } else {
            JString::default()
          },
        )
      } else {
        (JString::default(), JString::default())
      };

      let headers = response.headers();
      let obj = env.new_object("java/util/HashMap", "()V", &[])?;
      let response_headers = {
        let headers_map = JMap::from_env(env, &obj)?;
        for (name, value) in headers.iter() {
          // WebResourceResponse will automatically generate Content-Type and
          // Content-Length headers so we should skip them to avoid duplication.
          if name == CONTENT_TYPE || name == CONTENT_LENGTH {
            continue;
          }
          let key = env.new_string(name)?;
          let value = env.new_string(value.to_str().unwrap_or_default())?;
          headers_map.put(env, &key, &value)?;
        }
        headers_map
      };

      let bytes = response.body();

      let byte_array_input_stream = env.find_class("java/io/ByteArrayInputStream")?;
      let byte_array = env.byte_array_from_slice(bytes)?;
      let stream = env.new_object(byte_array_input_stream, "([B)V", &[(&byte_array).into()])?;

      let reason_phrase = env.new_string(reason_phrase)?;

      let web_resource_response_class = env.find_class("android/webkit/WebResourceResponse")?;
      let web_resource_response = env.new_object(
        web_resource_response_class,
        "(Ljava/lang/String;Ljava/lang/String;ILjava/lang/String;Ljava/util/Map;Ljava/io/InputStream;)V",
        &[(&mime_type).into(), (&encoding).into(), status_code.into(), (&reason_phrase).into(), (&response_headers).into(), (&stream).into()],
      )?;

      return Ok(*web_resource_response);
    }
  }

  Ok(*JObject::null())
}

#[allow(non_snake_case)]
pub unsafe fn wryCreate(env: JNIEnv, _: JClass) {
  let mut main_pipe = MainPipe { env };

  let looper = ThreadLooper::for_thread().unwrap();

  looper
    .add_fd_with_callback(MAIN_PIPE[0].as_fd(), FdEvent::INPUT, move |fd, _event| {
      let size = std::mem::size_of::<bool>();
      let mut wake = false;
      if libc::read(fd.as_raw_fd(), &mut wake as *mut _ as *mut _, size) == size as libc::ssize_t {
        // A failed Java operation must not permanently remove the process-wide
        // queue callback. A later Activity may recover with a fresh WebView.
        if main_pipe.recv().is_err() {
          eprintln!("Android WebView operation failed");
          let _ = main_pipe.env.exception_clear();
        }
        true
      } else {
        // unregister itself
        false
      }
    })
    .unwrap();
}

#[allow(non_snake_case)]
pub unsafe fn onWebviewDestroy(mut env: JNIEnv, _: JClass, activity: JObject, webview_id: JString) {
  let activity_id = env
    .call_method(&activity, "getId", "()I", &[])
    .unwrap()
    .i()
    .unwrap();

  let webview_id = env
    .get_string(&webview_id)
    .unwrap()
    .to_string_lossy()
    .to_string();

  let is_changing_configurations = env
    .call_method(&activity, "isChangingConfigurations", "()Z", &[])
    .unwrap()
    .z()
    .unwrap();

  // This JNI callback already runs on Android's main thread. Cleanup must
  // finish before a new Activity is registered, and must not enqueue into the
  // bounded queue that only this same thread can drain.
  if !is_changing_configurations {
    super::main_pipe::destroy_activity(activity_id, &webview_id);
  } else {
    // Keep the logical window, but retire this particular Java view before
    // another Activity can install callbacks for the same logical label.
    super::main_pipe::retire_activity_view(activity_id);
  }
}

#[allow(non_snake_case)]
pub unsafe fn handleRequest(
  mut env: JNIEnv,
  _: JClass,
  webview_id: JString,
  request: JObject,
  is_document_start_script_enabled: jboolean,
) -> jobject {
  match handle_request(
    &mut env,
    webview_id,
    request,
    is_document_start_script_enabled,
  ) {
    Ok(response) => response,
    Err(_e) => {
      #[cfg(feature = "tracing")]
      tracing::warn!("Failed to handle request: {_e}");
      JObject::null().as_raw()
    }
  }
}

#[allow(non_snake_case)]
pub unsafe fn shouldOverride(
  mut env: JNIEnv,
  _: JClass,
  webview_id: JString,
  url: JString,
) -> jboolean {
  match env.get_string(&url) {
    Ok(url) => {
      let url = url.to_string_lossy().to_string();

      let Ok(webview_id) = env.get_string(&webview_id) else {
        return false.into();
      };
      let webview_id = webview_id.to_str().ok().unwrap_or_default();

      let Some(callbacks) = callbacks::resolve(webview_id) else {
        // A retired Java view must not start another navigation.
        return true.into();
      };
      callbacks
        .navigation
        .as_ref()
        // We negate the result of the function because the logic for the android
        // client is different from how the navigation_handler is defined.
        //
        // https://developer.android.com/reference/android/webkit/WebViewClient#shouldOverrideUrlLoading(android.webkit.WebView,%20android.webkit.WebResourceRequest)
        .map(|f| {
          let handler = f.lock().unwrap();
          !callbacks::is_current(webview_id, &callbacks) || !(handler.handler)(url)
        })
        .unwrap_or_default()
    }
    Err(_e) => {
      #[cfg(feature = "tracing")]
      tracing::warn!("Failed to parse JString: {_e}");
      false
    }
  }
  .into()
}

#[allow(non_snake_case)]
pub unsafe fn onEval(mut env: JNIEnv, _: JClass, webview_id: JString, id: jint, result: JString) {
  let Ok(webview_id) = env.get_string(&webview_id) else {
    return;
  };
  // Eval ids identify their original callback, but the Java view must still
  // have authority to deliver it. Always release one-shot callback storage.
  let active = callbacks::resolve(webview_id.to_str().ok().unwrap_or_default()).is_some();
  let callback = EVAL_CALLBACKS
    .get_or_init(Default::default)
    .lock()
    .unwrap()
    .remove(&id);
  if !active {
    return;
  }
  match env.get_string(&result) {
    Ok(result) => {
      if let Some((_activity_id, cb)) = callback {
        cb(result.into());
      }
    }
    Err(_e) => {
      #[cfg(feature = "tracing")]
      tracing::warn!("Failed to parse JString: {_e}");
    }
  }
}

pub unsafe fn ipc(mut env: JNIEnv, _: JClass, webview_id: JString, url: JString, body: JString) {
  match (
    env.get_string(&url),
    env.get_string(&body),
    env.get_string(&webview_id),
  ) {
    (Ok(url), Ok(body), Ok(webview_id)) => {
      #[cfg(feature = "tracing")]
      let _span = tracing::info_span!(parent: None, "wry::ipc::handle").entered();

      let url = url.to_string_lossy().to_string();
      let body = body.to_string_lossy().to_string();
      let webview_id = webview_id.to_string_lossy().to_string();
      let Some(callbacks) = callbacks::resolve(&webview_id) else {
        return;
      };
      if let Some(ipc) = &callbacks.ipc {
        let ipc = ipc.lock().unwrap();
        if !callbacks::is_current(&webview_id, &callbacks) {
          return;
        }
        (ipc.handler)(Request::builder().uri(url).body(body).unwrap())
      }
    }
    (Err(_e), _, _) | (_, Err(_e), _) | (_, _, Err(_e)) => {
      #[cfg(feature = "tracing")]
      tracing::warn!("Failed to parse JString: {_e}")
    }
  }
}

#[allow(non_snake_case)]
pub unsafe fn handleReceivedTitle(mut env: JNIEnv, _: JClass, webview_id: JString, title: JString) {
  match (env.get_string(&title), env.get_string(&webview_id)) {
    (Ok(title), Ok(webview_id)) => {
      let title = title.to_string_lossy().to_string();
      let webview_id = webview_id.to_string_lossy().to_string();
      let Some(callbacks) = callbacks::resolve(&webview_id) else {
        return;
      };
      if let Some(title_handler) = &callbacks.title {
        let title_handler = title_handler.lock().unwrap();
        if !callbacks::is_current(&webview_id, &callbacks) {
          return;
        }
        (title_handler.handler)(title)
      }
    }
    (Err(_e), _) | (_, Err(_e)) => {
      #[cfg(feature = "tracing")]
      tracing::warn!("Failed to parse JString: {_e}")
    }
  }
}

#[allow(non_snake_case)]
pub unsafe fn withAssetLoader(mut env: JNIEnv, _: JClass, webview_id: JString) -> jboolean {
  let Ok(webview_id) = env.get_string(&webview_id) else {
    return false.into();
  };
  let webview_id = webview_id.to_str().ok().unwrap_or_default();
  callbacks::resolve(webview_id)
    .is_some_and(|callbacks| callbacks.with_asset_loader)
    .into()
}

#[allow(non_snake_case)]
pub unsafe fn assetLoaderDomain(mut env: JNIEnv, _: JClass, webview_id: JString) -> jstring {
  let Ok(webview_id) = env.get_string(&webview_id) else {
    return env.new_string("wry.assets").unwrap().as_raw();
  };
  let webview_id = webview_id.to_str().ok().unwrap_or_default();
  let callbacks = callbacks::resolve(webview_id);
  let domain = callbacks
    .as_ref()
    .map(|callbacks| callbacks.asset_loader_domain.as_str())
    .unwrap_or("wry.assets");
  env.new_string(domain).unwrap().as_raw()
}

#[allow(non_snake_case)]
pub unsafe fn onPageLoading(mut env: JNIEnv, _: JClass, webview_id: JString, url: JString) {
  match (env.get_string(&url), env.get_string(&webview_id)) {
    (Ok(url), Ok(webview_id)) => {
      let url = url.to_string_lossy().to_string();
      let webview_id = webview_id.to_string_lossy().to_string();
      let Some(callbacks) = callbacks::resolve(&webview_id) else {
        return;
      };
      if let Some(on_load) = &callbacks.load {
        let on_load = on_load.lock().unwrap();
        if !callbacks::is_current(&webview_id, &callbacks) {
          return;
        }
        (on_load.handler)(PageLoadEvent::Started, url)
      }
    }
    (Err(_e), _) | (_, Err(_e)) => {
      #[cfg(feature = "tracing")]
      tracing::warn!("Failed to parse JString: {_e}")
    }
  }
}

#[allow(non_snake_case)]
pub unsafe fn onPageLoaded(mut env: JNIEnv, _: JClass, webview_id: JString, url: JString) {
  match (env.get_string(&url), env.get_string(&webview_id)) {
    (Ok(url), Ok(webview_id)) => {
      let url = url.to_string_lossy().to_string();
      let webview_id = webview_id.to_string_lossy().to_string();
      let Some(callbacks) = callbacks::resolve(&webview_id) else {
        return;
      };
      if let Some(on_load) = &callbacks.load {
        let on_load = on_load.lock().unwrap();
        if !callbacks::is_current(&webview_id, &callbacks) {
          return;
        }
        (on_load.handler)(PageLoadEvent::Finished, url)
      }
    }
    (Err(_e), _) | (_, Err(_e)) => {
      #[cfg(feature = "tracing")]
      tracing::warn!("Failed to parse JString: {_e}")
    }
  }
}
