# THIS FILE IS AUTO-GENERATED. DO NOT MODIFY!!

# Copyright 2020-2023 Tauri Programme within The Commons Conservancy
# SPDX-License-Identifier: Apache-2.0
# SPDX-License-Identifier: MIT

-keep class org.ratspeak.android.* {
  native <methods>;
}

-keep class org.ratspeak.android.WryActivity {
  public <init>(...);

  void setWebView(org.ratspeak.android.RustWebView);
  java.lang.Class getAppClass(...);
  int getId();
  java.lang.String getVersion();
  int startActivity(...);
}

-keep class org.ratspeak.android.Ipc {
  public <init>(...);

  @android.webkit.JavascriptInterface public <methods>;
}

-keep class org.ratspeak.android.RustWebView {
  public <init>(...);

  void loadUrlMainThread(...);
  void loadHTMLMainThread(...);
  void evalScript(...);
}

-keep class org.ratspeak.android.RustWebChromeClient,org.ratspeak.android.RustWebViewClient {
  public <init>(...);
}
