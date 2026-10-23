use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::Stream;
use tokio::sync::{mpsc, watch};
use tokio_stream::wrappers::ReceiverStream;

use super::macos_pasteboard as pb;
use super::task::ClipboardPortal;

/// macOS has no clipboard change notifications, so `changeCount` is polled.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

struct EchoSuppressor {
    expected_change_count: Option<isize>,
    is_writing: bool,
}

impl EchoSuppressor {
    fn new() -> Self {
        Self {
            expected_change_count: None,
            is_writing: false,
        }
    }

    fn prepare_write(&mut self) {
        self.is_writing = true;
    }

    fn record_write(&mut self, change_count: isize) {
        self.expected_change_count = Some(change_count);
        self.is_writing = false;
    }

    fn check_and_clear_echo(&mut self, current: isize) -> bool {
        if self.is_writing {
            return true;
        }
        if let Some(expected) = self.expected_change_count.take() {
            return expected == current;
        }
        false
    }
}

pub struct MacClipboardPortal {
    owner_changed_rx: Arc<tokio::sync::Mutex<Option<mpsc::Receiver<()>>>>,
    suppressor: Arc<Mutex<EchoSuppressor>>,
}

impl Default for MacClipboardPortal {
    fn default() -> Self {
        Self::new()
    }
}

impl MacClipboardPortal {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel(4);
        let suppressor = Arc::new(Mutex::new(EchoSuppressor::new()));
        let suppressor_clone = suppressor.clone();

        std::thread::spawn(move || {
            log::info!("macOS clipboard monitor started.");
            let mut last_seen = pb::change_count();

            loop {
                std::thread::sleep(POLL_INTERVAL);
                if tx.is_closed() {
                    log::info!(
                        "Clipboard portal owner_changed channel closed, terminating monitor."
                    );
                    break;
                }

                let current = pb::change_count();
                if current == last_seen {
                    continue;
                }
                last_seen = current;

                log::info!("NSPasteboard changeCount changed to {}", current);

                let is_echo = suppressor_clone
                    .lock()
                    .map(|mut supp| supp.check_and_clear_echo(current))
                    .unwrap_or(false);
                if is_echo {
                    log::info!("NSPasteboard change suppressed as our own write (echo)");
                    continue;
                }

                if crate::clipboard::file_task::FILE_IS_WRITING.load(Ordering::SeqCst) {
                    log::info!("NSPasteboard change suppressed as file is currently writing");
                    continue;
                }

                let expected_file_seq =
                    crate::clipboard::file_task::FILE_EXPECTED_SEQ.load(Ordering::SeqCst);
                if expected_file_seq != 0 {
                    let current_seq = current as u32;
                    if expected_file_seq == current_seq {
                        crate::clipboard::file_task::FILE_EXPECTED_SEQ.store(0, Ordering::SeqCst);
                        log::info!("NSPasteboard change suppressed as file transfer echo");
                        continue;
                    } else if current_seq > expected_file_seq
                        || (expected_file_seq - current_seq > 1000)
                    {
                        // Clear if we missed it
                        crate::clipboard::file_task::FILE_EXPECTED_SEQ.store(0, Ordering::SeqCst);
                    }
                }

                log::info!("NSPasteboard change accepted as external change");

                let _ = super::LOCAL_CLIPBOARD_CHANGED.send(());
                if tx.blocking_send(()).is_err() {
                    log::info!(
                        "Clipboard portal owner_changed channel closed, terminating monitor."
                    );
                    break;
                }
            }
        });

        Self {
            owner_changed_rx: Arc::new(tokio::sync::Mutex::new(Some(rx))),
            suppressor,
        }
    }
}

impl ClipboardPortal for MacClipboardPortal {
    fn eager_fetch(&self) -> bool {
        // Like Windows, we use eager fetching since NSPasteboard has no cheap lazy provider here.
        true
    }

    fn receive_selection_owner_changed(
        &self,
    ) -> Pin<
        Box<dyn std::future::Future<Output = Pin<Box<dyn Stream<Item = ()> + Send>>> + Send + '_>,
    > {
        Box::pin(async move {
            let rx = self.owner_changed_rx.lock().await.take().unwrap();
            Box::pin(ReceiverStream::new(rx)) as Pin<Box<dyn Stream<Item = ()> + Send>>
        })
    }

    fn receive_selection_transfer(
        &self,
    ) -> Pin<
        Box<dyn std::future::Future<Output = Pin<Box<dyn Stream<Item = ()> + Send>>> + Send + '_>,
    > {
        Box::pin(async move {
            // Because we use eager fetching, we never emit events from this stream.
            let (_tx, rx) = tokio::sync::mpsc::channel(1);
            Box::pin(ReceiverStream::new(rx)) as Pin<Box<dyn Stream<Item = ()> + Send>>
        })
    }

    fn selection_write<'a>(
        &'a self,
        items: Vec<super::task::ClipboardItem>,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>> {
        let suppressor = self.suppressor.clone();
        Box::pin(async move {
            let mut text = None;
            let mut html = None;
            let mut png = None;

            for item in items {
                match item.mime_type.as_str() {
                    "text/plain" => text = String::from_utf8(item.data).ok(),
                    "text/html" => html = Some(item.data),
                    "image/png" => {
                        super::validate_image_dimensions(&item.data)?;
                        png = Some(item.data);
                    }
                    _ => {}
                }
            }

            if text.is_none() && html.is_none() && png.is_none() {
                return Ok(());
            }

            tokio::task::spawn_blocking(move || {
                if let Ok(mut supp) = suppressor.lock() {
                    supp.prepare_write();
                }
                let change_count = pb::write_items(text, html, png);
                if let Ok(mut supp) = suppressor.lock() {
                    supp.record_write(change_count);
                }
            })
            .await
            .map_err(|e| e.to_string())
        })
    }

    fn selection_read<'a>(
        &'a self,
        mime: &'a str,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<Vec<u8>, String>> + Send + 'a>> {
        Box::pin(async move {
            log::info!("selection_read() invoked for mime {}", mime);
            let data = if mime == "text/plain" {
                tokio::task::spawn_blocking(pb::read_string)
                    .await
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| "No text on clipboard".to_string())?
                    .into_bytes()
            } else if mime == "text/html" {
                tokio::task::spawn_blocking(pb::read_html)
                    .await
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| "No HTML on clipboard".to_string())?
            } else if mime == "image/png" || mime == "image/jpeg" {
                tokio::task::spawn_blocking(pb::read_png)
                    .await
                    .map_err(|e| e.to_string())??
                    .ok_or_else(|| "No image on clipboard".to_string())?
            } else {
                return Err(format!("Unsupported MIME type: {}", mime));
            };

            // Apply limit (account for 13 bytes framing overhead)
            let max_payload = (crate::clipboard::transport::MAX_CLIPBOARD_FRAME_SIZE - 13) as usize;
            if data.len() > max_payload {
                return Err(format!(
                    "Clipboard data exceeds {}MB limit",
                    crate::clipboard::transport::MAX_CLIPBOARD_FRAME_SIZE / (1024 * 1024)
                ));
            }
            log::info!("data successfully read: {} bytes", data.len());
            Ok(data)
        })
    }

    fn set_selection<'a>(
        &'a self,
        _mime: &'a str,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            // No-op for macOS since we don't own the selection without data (no lazy providers).
            Ok(())
        })
    }

    fn get_available_mime_types(
        &self,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<Vec<String>, String>> + Send + '_>> {
        Box::pin(async move {
            tokio::task::spawn_blocking(|| {
                // Files are handled by file_task. Finder also adds the file name as a string
                // and the file icon as TIFF, which must not be sent as text/image.
                if pb::has_file_urls() {
                    return Vec::new();
                }
                let mut avail = Vec::new();
                if pb::has_string() {
                    avail.push("text/plain".to_string());
                }
                if pb::has_html() {
                    avail.push("text/html".to_string());
                }
                if pb::has_image() {
                    avail.push("image/png".to_string());
                }
                avail
            })
            .await
            .map_err(|e| e.to_string())
        })
    }
}

pub fn init_clipboard_task() -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let (tx, rx) = watch::channel(Some(()));
        // We hold the tx so it doesn't close, even though it never updates.
        let _tx = tx;
        super::task::ClipboardTask::run_with_factory(rx, |_| async {
            Some(Box::new(MacClipboardPortal::new()) as Box<dyn ClipboardPortal>)
        })
        .await;
    })
}
