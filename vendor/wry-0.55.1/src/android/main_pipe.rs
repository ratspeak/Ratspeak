// Copyright 2020-2023 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

use crate::{Error, InitializationScript, RGBA};
use crossbeam_channel::*;
use jni::{
  errors::Result as JniResult,
  objects::{GlobalRef, JMap, JObject, JString},
  JNIEnv, JavaVM,
};
use once_cell::sync::Lazy;
use std::{
  collections::BTreeMap,
  ffi::c_void,
  os::unix::prelude::*,
  sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
  },
};

use super::native_creation::NativeCreation;
use super::{find_class, EvalCallback, WebviewId, EVAL_CALLBACKS, EVAL_ID_GENERATOR, PACKAGE};

pub type ActivityId = i32;

static NEXT_ACTIVITY_GENERATION: AtomicU64 = AtomicU64::new(1);

type QueuedMessage = (ActivityId, Arc<Mutex<bool>>, WebViewMessage);

static CHANNEL: Lazy<(Sender<QueuedMessage>, Receiver<QueuedMessage>)> = Lazy::new(|| bounded(8));
pub static MAIN_PIPE: Lazy<[OwnedFd; 2]> = Lazy::new(|| {
  let mut pipe: [RawFd; 2] = Default::default();
  unsafe { libc::pipe(pipe.as_mut_ptr()) };
  unsafe { pipe.map(|fd| OwnedFd::from_raw_fd(fd)) }
});

#[derive(Clone)]
pub struct ActivityProxy {
  pub activity: GlobalRef,
  pub window_manager: GlobalRef,
  pub webview: Option<GlobalRef>,
  pub webchrome_client: GlobalRef,
  pub java_vm: *mut c_void,
  pub lifetime: Arc<Mutex<bool>>,
  // Copyable JNI handles carry this serial to reject an old Activity even
  // when Android reuses its integer id. Configuration changes retain it.
  pub generation: u64,
  pub creation: NativeCreation,
}

unsafe impl Send for ActivityProxy {}

impl ActivityProxy {
  pub fn new(
    vm: JavaVM,
    activity: GlobalRef,
    window_manager: GlobalRef,
    webchrome_client: GlobalRef,
  ) -> Self {
    Self {
      activity,
      window_manager,
      webview: None,
      webchrome_client,
      java_vm: vm.get_java_vm_pointer() as *mut _,
      lifetime: Arc::new(Mutex::new(true)),
      generation: NEXT_ACTIVITY_GENERATION.fetch_add(1, Ordering::Relaxed),
      creation: NativeCreation::new(NEXT_ACTIVITY_GENERATION.fetch_add(1, Ordering::Relaxed)),
    }
  }
}

static ACTIVITY_PROXY: once_cell::sync::Lazy<Mutex<BTreeMap<ActivityId, ActivityProxy>>> =
  Lazy::new(|| Mutex::new(BTreeMap::new()));

pub fn activity_proxy(id: ActivityId) -> Option<ActivityProxy> {
  ACTIVITY_PROXY.lock().unwrap().get(&id).cloned()
}

pub(crate) fn destroy_activity(id: ActivityId, webview_id: &WebviewId) {
  super::callbacks::retire(id);
  clear_eval_callbacks(id);
  if let Some(proxy) = activity_proxy(id) {
    // Construction only holds this guard while publishing Rust handler maps,
    // never while sending to the Android queue or invoking application JNI.
    let mut live = proxy.lifetime.lock().unwrap();
    *live = false;
    super::destroy_webview(id, proxy.generation, webview_id);
    ACTIVITY_PROXY.lock().unwrap().remove(&id);
  }
}

pub(crate) fn retire_activity_view(id: ActivityId) {
  super::callbacks::retire(id);
  clear_eval_callbacks(id);
  if let Some(proxy) = ACTIVITY_PROXY.lock().unwrap().get_mut(&id) {
    proxy.webview = None;
    proxy.creation.retire();
  }
}

fn clear_eval_callbacks(activity_id: ActivityId) {
  if let Some(callbacks) = EVAL_CALLBACKS.get() {
    let retired = {
      let mut callbacks = callbacks.lock().unwrap();
      let ids: Vec<_> = callbacks
        .iter()
        .filter_map(|(id, (owner, _))| (*owner == activity_id).then_some(*id))
        .collect();
      ids
        .into_iter()
        .filter_map(|id| callbacks.remove(&id))
        .collect::<Vec<_>>()
    };
    // Dropping callback captures can wake waiters; never do it under the map lock.
    drop(retired);
  }
}

pub fn register_activity_proxy(
  vm: JavaVM,
  id: ActivityId,
  activity: GlobalRef,
  window_manager: GlobalRef,
  webchrome_client: GlobalRef,
) {
  super::callbacks::retire(id);
  clear_eval_callbacks(id);
  let mut activity_proxy = ACTIVITY_PROXY.lock().unwrap();
  if let Some(proxy) = activity_proxy.get_mut(&id) {
    proxy.activity = activity;
    proxy.window_manager = window_manager;
    proxy.webchrome_client = webchrome_client;
    proxy.java_vm = vm.get_java_vm_pointer() as *mut _;
    proxy.webview = None;
    proxy.creation = NativeCreation::new(NEXT_ACTIVITY_GENERATION.fetch_add(1, Ordering::Relaxed));
  } else {
    let proxy = ActivityProxy::new(vm, activity, window_manager, webchrome_client);
    activity_proxy.insert(id, proxy.clone());
  }
}

pub fn activity_id_for_window_manager(window_manager: JObject) -> Option<ActivityId> {
  for (activity_id, proxy) in ACTIVITY_PROXY.lock().unwrap().iter() {
    let vm = unsafe { JavaVM::from_raw(proxy.java_vm.cast()) }.unwrap();
    let mut env = vm.attach_current_thread_as_daemon().unwrap();
    let equals = env
      .call_method(
        proxy.window_manager.as_obj(),
        "equals",
        "(Ljava/lang/Object;)Z",
        &[(&window_manager).into()],
      )
      .and_then(|v| v.z())
      .unwrap_or_default();
    if equals {
      return Some(*activity_id);
    }
  }
  None
}

pub fn first_activity_id() -> Option<ActivityId> {
  ACTIVITY_PROXY.lock().unwrap().keys().next().cloned()
}

pub fn get_webview(activity_id: ActivityId) -> Option<GlobalRef> {
  ACTIVITY_PROXY
    .lock()
    .unwrap()
    .get(&activity_id)?
    .webview
    .as_ref()
    .cloned()
}

pub struct MainPipe<'a> {
  pub env: JNIEnv<'a>,
}

impl<'a> MainPipe<'a> {
  pub(crate) fn send(activity_id: ActivityId, message: WebViewMessage) {
    let Some(proxy) = activity_proxy(activity_id) else {
      return;
    };
    Self::send_for_lifetime(activity_id, proxy.lifetime, message);
  }

  pub(crate) fn send_for_lifetime(
    activity_id: ActivityId,
    lifetime: Arc<Mutex<bool>>,
    message: WebViewMessage,
  ) {
    if !activity_proxy(activity_id).is_some_and(|proxy| Arc::ptr_eq(&proxy.lifetime, &lifetime))
      || !*lifetime.lock().unwrap()
    {
      return;
    }
    // Never hold lifecycle/map locks while waiting for queue space. Receipt
    // checks the same token again after any intervening Activity destruction.
    let size = std::mem::size_of::<bool>();
    if CHANNEL.0.send((activity_id, lifetime, message)).is_ok() {
      unsafe {
        libc::write(
          MAIN_PIPE[1].as_raw_fd(),
          &true as *const _ as *const _,
          size,
        )
      };
    }
  }

  pub fn recv(&mut self) -> JniResult<()> {
    if let Ok((activity_id, lifetime, message)) = CHANNEL.1.recv() {
      return self.handle_message(activity_id, &lifetime, message);
    }
    Ok(())
  }

  // Android's UI Looper owns this entry point. Configuration recreation can
  // invoke it directly instead of enqueueing into its own bounded queue.
  pub(crate) fn handle_message(
    &mut self,
    activity_id: ActivityId,
    lifetime: &Arc<Mutex<bool>>,
    message: WebViewMessage,
  ) -> JniResult<()> {
    // Stale messages drop their synchronous reply senders promptly, so a
    // request cannot hang or dispatch against a replacement using the same id.
    if activity_proxy(activity_id).is_some_and(|proxy| Arc::ptr_eq(&proxy.lifetime, lifetime))
      && *lifetime.lock().unwrap()
    {
      match message {
        WebViewMessage::CreateWebView(attrs) => {
          let native_generation = attrs.native_generation;
          let Some((activity, web_chrome_client)) = ACTIVITY_PROXY
            .lock()
            .unwrap()
            .get_mut(&activity_id)
            .filter(|proxy| Arc::ptr_eq(&proxy.lifetime, &attrs.activity_lifetime))
            .filter(|proxy| proxy.creation.epoch == native_generation)
            .and_then(|proxy| {
              proxy
                .creation
                .begin(native_generation)
                .then(|| (proxy.activity.clone(), proxy.webchrome_client.clone()))
            })
          else {
            #[cfg(debug_assertions)]
            eprintln!("no activity found for activity id: {}", activity_id);
            return Ok(());
          };
          let Some(callback_key) = super::callbacks::register(activity_id, &attrs.handler_key)
          else {
            retire_activity_view(activity_id);
            return Err(jni::errors::Error::NullPtr(
              "native WebView request handler",
            ));
          };
          let mut phase = "constructor";
          let result: JniResult<()> = (|| {
            let CreateWebViewAttributes {
              url,
              html,
              #[cfg(any(debug_assertions, feature = "devtools"))]
              devtools,
              transparent,
              background_color,
              headers,
              on_webview_created,
              autoplay,
              user_agent,
              initialization_scripts,
              id,
              javascript_disabled,
              ..
            } = attrs;

            let string_class = self.env.find_class("java/lang/String")?;
            let initialization_scripts_array = self.env.new_object_array(
              initialization_scripts.len() as i32,
              string_class,
              self.env.new_string("")?,
            )?;
            for (i, init_script) in initialization_scripts.into_iter().enumerate() {
              self.env.set_object_array_element(
                &initialization_scripts_array,
                i as i32,
                self.env.new_string(init_script.script)?,
              )?;
            }
            let id = self.env.new_string(id)?;
            let callback_key = self.env.new_string(callback_key)?;
            // Create webview
            let rust_webview_class = find_class(
              &mut self.env,
              &activity,
              format!("{}/RustWebView", PACKAGE.get().unwrap()),
            )?;
            let webview = self.env.new_object(
              &rust_webview_class,
              "(Landroid/content/Context;[Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)V",
              &[
                (&activity).into(),
                (&initialization_scripts_array).into(),
                (&id).into(),
                (&callback_key).into(),
              ],
            )?;
            // get settings
            let web_settings = self
              .env
              .call_method(
                &webview,
                "getSettings",
                "()Landroid/webkit/WebSettings;",
                &[],
              )?
              .l()?;
            // set media autoplay
            self.env.call_method(
              &web_settings,
              "setMediaPlaybackRequiresUserGesture",
              "(Z)V",
              &[(!autoplay).into()],
            )?;
            // set user-agent
            if let Some(user_agent) = user_agent {
              let user_agent = self.env.new_string(user_agent)?;
              self.env.call_method(
                &web_settings,
                "setUserAgentString",
                "(Ljava/lang/String;)V",
                &[(&user_agent).into()],
              )?;
            }

            // disable javascript
            if javascript_disabled {
              self.env.call_method(
                &web_settings,
                "setJavaScriptEnabled",
                "(Z)V",
                &[false.into()],
              )?;
            }

            let webview_class_name = format!("{}/RustWebView", PACKAGE.get().unwrap());
            phase = "app_setup";
            self.env.call_method(
              &activity,
              "setWebView",
              format!("(L{webview_class_name};)V"),
              &[(&webview).into()],
            )?;
            // Enable devtools
            #[cfg(any(debug_assertions, feature = "devtools"))]
            self.env.call_static_method(
              &rust_webview_class,
              "setWebContentsDebuggingEnabled",
              "(Z)V",
              &[devtools.into()],
            )?;
            if transparent {
              set_background_color(&mut self.env, &webview, (0, 0, 0, 0))?;
            } else if let Some(color) = background_color {
              set_background_color(&mut self.env, &webview, color)?;
            }
            // Create and set webview client
            phase = "webview_client";
            let client_class_name = format!("{}/RustWebViewClient", PACKAGE.get().unwrap());
            let rust_webview_client_class =
              find_class(&mut self.env, &activity, client_class_name.clone())?;
            let webview_client = self.env.new_object(
              &rust_webview_client_class,
              format!("(L{webview_class_name};Landroid/content/Context;)V"),
              &[(&webview).into(), (&activity).into()],
            )?;
            self.env.call_method(
              &webview,
              "setWebViewClient",
              "(Landroid/webkit/WebViewClient;)V",
              &[(&webview_client).into()],
            )?;
            // set webchrome client
            self.env.call_method(
              &webview,
              "setWebChromeClient",
              "(Landroid/webkit/WebChromeClient;)V",
              &[web_chrome_client.as_obj().into()],
            )?;

            // Add javascript interface (IPC)
            phase = "ipc";
            let ipc_class = find_class(
              &mut self.env,
              &activity,
              format!("{}/Ipc", PACKAGE.get().unwrap()),
            )?;
            let ipc = self.env.new_object(
              ipc_class,
              format!("(L{webview_class_name};L{client_class_name};)V"),
              &[(&webview).into(), (&webview_client).into()],
            )?;
            let ipc_str = self.env.new_string("ipc")?;
            self.env.call_method(
              &webview,
              "addJavascriptInterface",
              "(Ljava/lang/Object;Ljava/lang/String;)V",
              &[(&ipc).into(), (&ipc_str).into()],
            )?;

            // Set content view
            phase = "content_view";
            self.env.call_method(
              &activity,
              "setContentView",
              "(Landroid/view/View;)V",
              &[(&webview).into()],
            )?;

            if let Some(on_webview_created) = on_webview_created {
              phase = "creation_hook";
              on_webview_created(super::Context {
                env: &mut self.env,
                activity: &activity,
                webview: &webview,
              })?;
            }

            // Install the interceptor and IPC before initiating any navigation.
            // A warm WebView can otherwise send the local asset URL to the
            // network before its custom-protocol client has been attached.
            phase = "navigation";
            if let Some(u) = url {
              let url = self.env.new_string(u)?;
              load_url(&mut self.env, &webview, &url, headers, true)?;
            } else if let Some(h) = html {
              let html = self.env.new_string(h)?;
              load_html(&mut self.env, &webview, &html)?;
            }
            phase = "publication";
            let global_webview = self.env.new_global_ref(&webview)?;
            let published = ACTIVITY_PROXY
              .lock()
              .unwrap()
              .get_mut(&activity_id)
              .filter(|proxy| Arc::ptr_eq(&proxy.lifetime, lifetime))
              .is_some_and(|proxy| {
                if !proxy.creation.complete(native_generation) {
                  return false;
                }
                proxy.webview = Some(global_webview);
                true
              });
            if published {
              phase = "ready_callback";
              self.env.call_method(
                &activity,
                "onWebViewReady",
                "(Landroid/webkit/WebView;)V",
                &[(&webview).into()],
              )?;
            }
            Ok(())
          })();
          if result.is_err() {
            eprintln!("Android WebView creation failed at {phase}");
            if activity_proxy(activity_id)
              .is_some_and(|proxy| proxy.creation.epoch == native_generation)
            {
              retire_activity_view(activity_id);
            }
          }
          result?;
        }
        WebViewMessage::Eval(script, callback) => {
          if let Some(webview) = get_webview(activity_id) {
            let id = EVAL_ID_GENERATOR.next() as i32;

            #[cfg(feature = "tracing")]
            let span = std::sync::Mutex::new(Some(SendEnteredSpan(
              tracing::debug_span!("wry::eval").entered(),
            )));

            EVAL_CALLBACKS
              .get_or_init(Default::default)
              .lock()
              .unwrap()
              .insert(
                id,
                (
                  activity_id,
                  Box::new(move |result| {
                    #[cfg(feature = "tracing")]
                    span.lock().unwrap().take();

                    if let Some(callback) = &callback {
                      callback(result);
                    }
                  }),
                ),
              );

            let s = self.env.new_string(script)?;
            self.env.call_method(
              webview.as_obj(),
              "evalScript",
              "(ILjava/lang/String;)V",
              &[id.into(), (&s).into()],
            )?;
          }
        }
        WebViewMessage::SetBackgroundColor(background_color) => {
          if let Some(webview) = get_webview(activity_id) {
            set_background_color(&mut self.env, webview.as_obj(), background_color)?;
          }
        }
        WebViewMessage::GetWebViewVersion(tx) => {
          if let Some(activity) = activity_proxy(activity_id).map(|p| p.activity.clone()) {
            match self
              .env
              .call_method(activity, "getVersion", "()Ljava/lang/String;", &[])
              .and_then(|v| v.l())
              .and_then(|s| {
                let s = JString::from(s);
                self
                  .env
                  .get_string(&s)
                  .map(|v| v.to_string_lossy().to_string())
              }) {
              Ok(version) => {
                let _ = tx.send(Ok(version));
              }
              Err(e) => {
                let _ = tx.send(Err(e.into()));
              }
            }
          } else {
            let _ = tx.send(Err(Error::ActivityNotFound));
          }
        }
        WebViewMessage::GetUrl(tx) => {
          if let Some(webview) = get_webview(activity_id) {
            let url = self
              .env
              .call_method(webview.as_obj(), "getUrl", "()Ljava/lang/String;", &[])
              .and_then(|v| v.l())
              .and_then(|s| {
                let s = JString::from(s);
                self
                  .env
                  .get_string(&s)
                  .map(|v| v.to_string_lossy().to_string())
              })
              .unwrap_or_default();

            let _ = tx.send(url);
          }
        }
        WebViewMessage::Jni(f) => {
          match activity_proxy(activity_id).map(|p| (p.activity.clone(), p.webview.clone())) {
            Some((activity, Some(webview))) => {
              f(&mut self.env, &activity, webview.as_obj());
            }
            Some((activity, None)) => {
              f(&mut self.env, &activity, &JObject::null());
            }
            _ => {
              f(&mut self.env, &JObject::null(), &JObject::null());
            }
          }
        }
        WebViewMessage::LoadUrl(url, headers) => {
          if let Some(webview) = get_webview(activity_id) {
            let url = self.env.new_string(url)?;
            load_url(&mut self.env, webview.as_obj(), &url, headers, false)?;
          }
        }
        WebViewMessage::ClearAllBrowsingData => {
          if let Some(webview) = get_webview(activity_id) {
            self
              .env
              .call_method(webview, "clearAllBrowsingData", "()V", &[])?;
          }
        }
        WebViewMessage::LoadHtml(html) => {
          if let Some(webview) = get_webview(activity_id) {
            let html = self.env.new_string(html)?;
            load_html(&mut self.env, webview.as_obj(), &html)?;
          }
        }
        WebViewMessage::Reload => {
          if let Some(webview) = get_webview(activity_id) {
            reload(&mut self.env, webview.as_obj())?;
          }
        }
        WebViewMessage::GetCookies(tx, url) => {
          if let Some(webview) = get_webview(activity_id) {
            let url = self.env.new_string(url)?;
            let cookies = self
              .env
              .call_method(
                webview,
                "getCookies",
                "(Ljava/lang/String;)Ljava/lang/String;",
                &[(&url).into()],
              )
              .and_then(|v| v.l())
              .and_then(|s| {
                let s = JString::from(s);
                self
                  .env
                  .get_string(&s)
                  .map(|v| v.to_string_lossy().to_string())
              })
              .unwrap_or_default();

            let _ = tx.send(
              cookies
                .split("; ")
                .flat_map(|c| cookie::Cookie::parse(c.to_string()))
                .collect(),
            );
          }
        }
      }
    }
    Ok(())
  }
}

fn load_url<'a>(
  env: &mut JNIEnv<'a>,
  webview: &JObject<'a>,
  url: &JString<'a>,
  headers: Option<http::HeaderMap>,
  main_thread: bool,
) -> JniResult<()> {
  let function = if main_thread {
    "loadUrlMainThread"
  } else {
    "loadUrl"
  };
  if let Some(headers) = headers {
    let obj = env.new_object("java/util/HashMap", "()V", &[])?;
    let headers_map = {
      let headers_map = JMap::from_env(env, &obj)?;
      for (name, value) in headers.iter() {
        let key = env.new_string(name)?;
        let value = env.new_string(value.to_str().unwrap_or_default())?;
        headers_map.put(env, &key, &value)?;
      }
      headers_map
    };
    env.call_method(
      webview,
      function,
      "(Ljava/lang/String;Ljava/util/Map;)V",
      &[url.into(), (&headers_map).into()],
    )?;
  } else {
    env.call_method(webview, function, "(Ljava/lang/String;)V", &[url.into()])?;
  }
  Ok(())
}

fn load_html<'a>(env: &mut JNIEnv<'a>, webview: &JObject<'a>, html: &JString<'a>) -> JniResult<()> {
  env.call_method(
    webview,
    "loadHTMLMainThread",
    "(Ljava/lang/String;)V",
    &[html.into()],
  )?;
  Ok(())
}

fn reload<'a>(env: &mut JNIEnv<'a>, webview: &JObject<'a>) -> JniResult<()> {
  env.call_method(webview, "reload", "()V", &[])?;
  Ok(())
}

fn set_background_color<'a>(
  env: &mut JNIEnv<'a>,
  webview: &JObject<'a>,
  (r, g, b, a): RGBA,
) -> JniResult<()> {
  let color = (a as i32) << 24 | (r as i32) << 16 | (g as i32) << 8 | (b as i32);
  env.call_method(webview, "setBackgroundColor", "(I)V", &[color.into()])?;
  Ok(())
}

pub(crate) enum WebViewMessage {
  CreateWebView(CreateWebViewAttributes),
  Eval(String, Option<EvalCallback>),
  SetBackgroundColor(RGBA),
  GetWebViewVersion(Sender<Result<String, Error>>),
  GetUrl(Sender<String>),
  GetCookies(Sender<Vec<cookie::Cookie<'static>>>, String),
  Jni(Box<dyn FnOnce(&mut JNIEnv, &JObject, &JObject) + Send>),
  LoadUrl(String, Option<http::HeaderMap>),
  LoadHtml(String),
  Reload,
  ClearAllBrowsingData,
}

#[derive(Clone)]
pub(crate) struct CreateWebViewAttributes {
  pub handler_key: super::HandlerKey,
  pub activity_lifetime: Arc<Mutex<bool>>,
  pub native_generation: u64,
  pub id: String,
  pub url: Option<String>,
  pub html: Option<String>,
  #[cfg(any(debug_assertions, feature = "devtools"))]
  pub devtools: bool,
  pub transparent: bool,
  pub background_color: Option<RGBA>,
  pub headers: Option<http::HeaderMap>,
  pub autoplay: bool,
  pub on_webview_created:
    Option<Arc<dyn Fn(super::Context) -> JniResult<()> + Send + Sync + 'static>>,
  pub user_agent: Option<String>,
  pub initialization_scripts: Vec<InitializationScript>,
  pub javascript_disabled: bool,
}

// SAFETY: only use this when you are sure the span will be dropped on the same thread it was entered
#[cfg(feature = "tracing")]
struct SendEnteredSpan(tracing::span::EnteredSpan);

#[cfg(feature = "tracing")]
unsafe impl Send for SendEnteredSpan {}
