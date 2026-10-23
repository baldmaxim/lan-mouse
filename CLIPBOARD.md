# Lan Mouse Clipboard Architecture

This document describes the clipboard synchronization and file transfer implementation across Windows, macOS and Linux (Wayland).

## Architecture

The clipboard system is divided into two primary areas:
1. **Platform-Specific Portals (`src/clipboard/os_*.rs`)**: Handles native OS clipboard reading and writing, exposing a standardized `ClipboardPortal` trait.
2. **Task and Protocol (`src/clipboard/task.rs`, `file_task.rs`)**: Implements the network negotiation and payload streaming over the TCP connection established by the `service`.

### Linux (Wayland)
On Linux Wayland, clipboard integration is built on two primary layers:
- `AshpdClipboardPortal`: Uses the XDG Desktop Portal (`org.freedesktop.portal.Clipboard`) to listen for clipboard changes (`receive_selection_owner_changed` and `receive_selection_transfer`).
- `WlClipboardPortal`: Uses `wl-clipboard-rs` and `wl-paste` to read and write clipboard payloads. 
  - **Important constraint:** Because `wl-clipboard-rs` uses `wlr-data-control` (a protocol originally meant for clipboard managers), it can sometimes conflict with desktop environments like KDE Plasma if state isn't perfectly managed. For reading and writing, `AshpdClipboardPortal` currently delegates directly to a managed `WlClipboardPortal` instance to ensure robust handling.

### Windows
On Windows, clipboard is managed natively via Win32 API (`clipboard-win` crate).
- Windows uses a polling/listener loop to capture `WM_CLIPBOARDUPDATE` messages.
- **Echo Suppression:** When Lan Mouse programmatically writes to the Windows clipboard, it temporarily records the OS sequence number using `GetClipboardSequenceNumber()`. If a subsequent `WM_CLIPBOARDUPDATE` matches this sequence number, it is safely ignored to prevent infinite network echo loops.

### macOS
On macOS, clipboard is managed via `NSPasteboard` (`objc2-app-kit`, helpers in `macos_pasteboard.rs`).
- macOS has no clipboard notifications, so a thread polls `changeCount` every 250 ms.
- **Echo Suppression:** After a programmatic write, the `changeCount` returned by the write is recorded; the poller ignores the change that matches it. File writes use `FILE_IS_WRITING` / `FILE_EXPECTED_SEQ` (`changeCount` as `u32`) like on Windows.
- **Files:** Finder copy puts `public.file-url` items on the pasteboard. If any are present, text/image types are not reported (Finder also adds the file name and icon), and the files are sent by `file_task`. Received files are written as file URLs, so Finder Cmd+V pastes them.
- **Images:** PNG is read directly; if absent, TIFF is converted to PNG. Images are written as PNG.

## Protocol & Flow Control

The Lan Mouse clipboard protocol (v4) relies on a dedicated TCP stream multiplexed over the same port as the UDP events.

1. **Offer Generation**: When the local clipboard changes, Lan Mouse retrieves available MIME types and sends an `Offer` message containing the types to the remote.
2. **MIME Negotiation**: The remote receives the `Offer`, compares it against its supported types, and replies with a `Request` for the specific types it can parse.
3. **Data Transmission**: The sender reads the specific MIME types from the native clipboard and sends them back as a `Data` payload.

### File Transfers
Cross-platform file transfers are explicitly supported. Files bypass the normal memory-buffered `Data` transmission due to size constraints.
- Files are negotiated via a specialized `FileOffer` / `FileRequest` flow.
- Data is streamed in chunks via `FileChunk` messages, capped at 256 KB.
- To prevent network queue bloat and TCP stalls, a flow-control mechanism requires the receiver to send `FileChunkAck` messages. The sender will pause transmission if too many chunks are unacknowledged.
- **Performance constraints:** For optimal performance, the underlying TCP stream has `TCP_NODELAY` enabled. Failure to enable `TCP_NODELAY` results in severe latency bottlenecks due to Delayed ACKs interacting with Nagle's algorithm.

## Known Limitations
- GNOME (Mutter) famously refuses to implement the `wlr-data-control` Wayland protocol. Because of this, Wayland-native clipboard synchronization (including files and plaintext) using `wl-clipboard` will silently fail on GNOME. Users must currently rely on X11 fallback clipboards or wait for a full `ashpd` D-Bus clipboard implementation.
- KDE Plasma Wayland (Klipper) has strict constraints on clipboard ownership. If `wl-clipboard-rs` hangs or fails to relinquish ownership, KDE might silently reject rich MIME types (like HTML or Image/PNG) while continuing to accept Plaintext. 
