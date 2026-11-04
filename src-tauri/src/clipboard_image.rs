//! WebKitGTK hands a pasted image to the page as an empty paste (WebKit bug
//! 218519), so Linux reads it from the GTK clipboard instead.

/// The clipboard's image as PNG bytes; empty when it holds none.
#[tauri::command]
pub async fn read_clipboard_image(app: tauri::AppHandle) -> Result<tauri::ipc::Response, String> {
    let (reply, png) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD).request_image(move |_, pixbuf| {
            let _ = reply.send(pixbuf.and_then(|image| image.save_to_bufferv("png", &[]).ok()));
        });
    })
    .map_err(|_| "The clipboard is unavailable".to_string())?;
    let png = png.await.ok().flatten().unwrap_or_default();
    Ok(tauri::ipc::Response::new(png))
}
