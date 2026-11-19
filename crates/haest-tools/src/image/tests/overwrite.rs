//! Where GenerateImage puts its picture: over an image that is there, it
//! is replaced whole, never emptied first; and only where the model named
//! that very path.

use std::path::{Path, PathBuf};

use haest_core::fs::Fs;
use serde_json::json;

use super::{Rig, image_answer};
use crate::testkit::Write;

/// The model named an image the human has. The picture went over it with
/// a plain write, which on the real disk empties it first: a disk that
/// filled up partway left the human's image cut short. It asks for the
/// whole write there, and makes a new file with a plain one.
#[tokio::test]
async fn a_picture_over_an_image_that_is_there_replaces_it_whole() {
    let r = Rig::new();
    let art = Path::new("/p/art.jpg");
    r.ws.mem.write(art, b"the human's picture").await.unwrap();
    let over = r.run(json!({"prompt": "a blue square", "file_path": "art.jpg"}));
    let over = over.await;
    assert!(!over.is_error, "{}", over.content);
    let new = r.run(json!({"prompt": "a red square", "file_path": "new/art.jpg"}));
    let new = new.await;
    assert!(!new.is_error, "{}", new.content);
    let writes = [
        (Write::Whole, art.to_path_buf()),
        (Write::Plain, PathBuf::from("/p/new/art.jpg")),
    ];
    assert_eq!(r.ws.writes(), writes);
    assert_ne!(r.ws.mem.read(art).await.unwrap(), b"the human's picture");
}

/// The model asked for `art.png` and the answer was a JPEG: the tool saves
/// it as `art.jpg`, a name the model never gave, and replaced the human's
/// `art.jpg` with it. A name the tool chose now goes to a free one beside.
#[tokio::test]
async fn a_name_the_tool_switched_to_never_goes_over_a_file() {
    let r = Rig::new();
    let art = Path::new("/p/art.jpg");
    r.ws.mem.write(art, b"the human's photo").await.unwrap();
    let out = r.run(json!({"prompt": "a blue square", "file_path": "art.png"}));
    let out = out.await;
    assert!(!out.is_error, "{}", out.content);
    assert_eq!(r.ws.mem.read(art).await.unwrap(), b"the human's photo");
    let saved = Path::new("/p/art-2.jpg");
    assert_eq!(r.ws.mem.read(saved).await.unwrap(), super::JPEG);
    assert!(!r.ws.mem.exists(Path::new("/p/art.png")).await);
}

/// The human asks for a JPEG of `logo.png`, passed as the reference; the
/// answer is a PNG all the same. Saved under its own extension, it went
/// over the reference, the human's only logo.
#[tokio::test]
async fn a_picture_never_goes_over_its_reference_under_a_switched_name() {
    const PNG: [u8; 8] = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
    let r = Rig::new();
    let logo = Path::new("/p/logo.png");
    r.ws.mem.write(logo, &PNG).await.unwrap();
    *r.api.answer.lock().unwrap() = image_answer(&[PNG.as_slice(), b"new"].concat());
    let call = json!({
        "prompt": "the same logo, as a JPEG",
        "file_path": "logo.jpg",
        "reference_images": ["logo.png"]
    });
    let out = r.run(call).await;
    assert!(!out.is_error, "{}", out.content);
    assert_eq!(r.ws.mem.read(logo).await.unwrap(), PNG);
    let saved = r.ws.mem.read(Path::new("/p/logo-2.png")).await.unwrap();
    assert_eq!(saved, [PNG.as_slice(), b"new"].concat());
}
