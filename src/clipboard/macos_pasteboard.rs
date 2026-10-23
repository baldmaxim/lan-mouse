//! Thin helpers over the macOS general `NSPasteboard`.
//!
//! Every call is wrapped in an autorelease pool; NSPasteboard may be used from any thread.

use std::path::PathBuf;

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{
    NSPasteboard, NSPasteboardTypeFileURL, NSPasteboardTypeHTML, NSPasteboardTypePNG,
    NSPasteboardTypeString, NSPasteboardTypeTIFF, NSPasteboardWriting,
};
use objc2_foundation::{NSArray, NSData, NSString, NSURL};

type PbType = objc2_app_kit::NSPasteboardType;

macro_rules! pb_type {
    ($name:ident, $static:ident) => {
        fn $name() -> &'static PbType {
            // SAFETY: AppKit extern constant, initialised before use and lives forever.
            unsafe { $static }
        }
    };
}

pb_type!(type_string, NSPasteboardTypeString);
pb_type!(type_html, NSPasteboardTypeHTML);
pb_type!(type_png, NSPasteboardTypePNG);
pb_type!(type_tiff, NSPasteboardTypeTIFF);
pb_type!(type_file_url, NSPasteboardTypeFileURL);

fn has_type(pb: &NSPasteboard, types: &[&PbType]) -> bool {
    pb.availableTypeFromArray(&NSArray::from_slice(types))
        .is_some()
}

/// Current `changeCount` of the general pasteboard.
pub fn change_count() -> isize {
    autoreleasepool(|_| NSPasteboard::generalPasteboard().changeCount())
}

/// Plain text (`public.utf8-plain-text`).
pub fn read_string() -> Option<String> {
    autoreleasepool(|_| {
        let pb = NSPasteboard::generalPasteboard();
        pb.stringForType(type_string()).map(|s| s.to_string())
    })
}

/// Raw bytes of `public.html`.
pub fn read_html() -> Option<Vec<u8>> {
    autoreleasepool(|_| {
        let pb = NSPasteboard::generalPasteboard();
        pb.dataForType(type_html()).map(|d| d.to_vec())
    })
}

/// PNG bytes; falls back to TIFF converted to PNG.
pub fn read_png() -> Result<Option<Vec<u8>>, String> {
    let (png, tiff) = autoreleasepool(|_| {
        let pb = NSPasteboard::generalPasteboard();
        (
            pb.dataForType(type_png()).map(|d| d.to_vec()),
            pb.dataForType(type_tiff()).map(|d| d.to_vec()),
        )
    });
    if png.is_some() {
        return Ok(png);
    }
    let Some(tiff) = tiff else {
        return Ok(None);
    };
    let img = image::load_from_memory_with_format(&tiff, image::ImageFormat::Tiff)
        .map_err(|e| format!("Failed to parse TIFF: {}", e))?;
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png)
        .map_err(|e| format!("Failed to encode image to PNG: {}", e))?;
    Ok(Some(out.into_inner()))
}

pub fn has_string() -> bool {
    autoreleasepool(|_| has_type(&NSPasteboard::generalPasteboard(), &[type_string()]))
}

pub fn has_html() -> bool {
    autoreleasepool(|_| has_type(&NSPasteboard::generalPasteboard(), &[type_html()]))
}

/// True if a PNG or TIFF image is present.
pub fn has_image() -> bool {
    autoreleasepool(|_| {
        has_type(
            &NSPasteboard::generalPasteboard(),
            &[type_png(), type_tiff()],
        )
    })
}

pub fn has_file_urls() -> bool {
    autoreleasepool(|_| has_type(&NSPasteboard::generalPasteboard(), &[type_file_url()]))
}

/// Local paths of all `public.file-url` items (what Finder puts on Cmd+C).
pub fn read_file_urls() -> Vec<PathBuf> {
    autoreleasepool(|_| {
        let pb = NSPasteboard::generalPasteboard();
        let Some(items) = pb.pasteboardItems() else {
            return Vec::new();
        };
        items
            .to_vec()
            .into_iter()
            .filter_map(|item| {
                let url_str = item.stringForType(type_file_url())?;
                let url = NSURL::URLWithString(&url_str)?;
                if !url.isFileURL() {
                    return None;
                }
                url.path().map(|p| PathBuf::from(p.to_string()))
            })
            .collect()
    })
}

/// Replaces the pasteboard contents with the given representations.
/// Returns the `changeCount` after the write.
pub fn write_items(text: Option<String>, html: Option<Vec<u8>>, png: Option<Vec<u8>>) -> isize {
    autoreleasepool(|_| {
        let pb = NSPasteboard::generalPasteboard();
        pb.clearContents();
        if let Some(text) = text {
            pb.setString_forType(&NSString::from_str(&text), type_string());
        }
        if let Some(html) = html {
            pb.setData_forType(Some(&NSData::with_bytes(&html)), type_html());
        }
        if let Some(png) = png {
            pb.setData_forType(Some(&NSData::with_bytes(&png)), type_png());
        }
        pb.changeCount()
    })
}

/// Replaces the pasteboard contents with file URLs (Finder can paste them).
/// Returns the `changeCount` after the write.
pub fn write_file_urls(paths: &[PathBuf]) -> isize {
    autoreleasepool(|_| {
        let urls: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = paths
            .iter()
            .filter_map(|p| p.to_str())
            .map(|s| ProtocolObject::from_retained(NSURL::fileURLWithPath(&NSString::from_str(s))))
            .collect();
        let pb = NSPasteboard::generalPasteboard();
        pb.clearContents();
        pb.writeObjects(&NSArray::from_retained_slice(&urls));
        pb.changeCount()
    })
}
