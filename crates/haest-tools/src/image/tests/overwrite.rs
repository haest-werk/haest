//! Where GenerateImage puts its picture: over an image that is there, it
//! is replaced whole, never emptied first.

use std::path::{Path, PathBuf};

use haest_core::fs::Fs;
use serde_json::json;

use super::Rig;
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
