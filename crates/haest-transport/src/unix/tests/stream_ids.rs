//! The socket and the discovery file are named after the stream id, which
//! is the backend's: one that is not a plain file name makes nothing.

use std::{path::PathBuf, time::Duration};

use haest_session::Hub;
use haest_wire::Origin;

use super::{Dir, UnixSocket};
use crate::{
    Grant, TransportError,
    testing::{answers, session},
};

/// An id that held a `/` (a `..`, a dir, an absolute path) put the socket
/// and the discovery file outside the control dir, renamed the discovery
/// file over a `.json` file of that name, and removed that file when the
/// transport ended. An empty id named `.json`, which `running_sessions`
/// never lists, and `.` named `..json`, whose temp file and socket path
/// (`with_extension`) are the dir's parent: `attach` failed as a write to
/// that dir. `attach` refuses such an id and touches nothing.
#[tokio::test]
async fn a_stream_id_that_is_not_a_file_name_is_refused() {
    let dir = Dir::new("sid");
    let ctl = dir.0.join("ctl");
    std::fs::create_dir_all(ctl.join("sub")).unwrap();
    let absolute = dir.0.join("abs");
    let cases: [(String, PathBuf); 3] = [
        ("../notes".into(), dir.0.join("notes.json")),
        ("sub/notes".into(), ctl.join("sub/notes.json")),
        (
            absolute.to_string_lossy().into_owned(),
            dir.0.join("abs.json"),
        ),
    ];
    for (sid, victim) in &cases {
        std::fs::write(victim, "the user's own").unwrap();
        let (s, _) = session(&answers(&[]), Hub::new(sid.as_str()));
        let attached = UnixSocket::new(&ctl).attach(s.clone(), Grant::watch(Origin::Agent));
        let refused = attached.as_ref().err().map(ToString::to_string);
        drop(attached);
        tokio::time::sleep(Duration::from_millis(50)).await;
        let left = std::fs::read_to_string(victim);
        assert_eq!(left.ok().as_deref(), Some("the user's own"), "{sid}");
        assert!(refused.is_some_and(|e| e.contains(sid.as_str())), "{sid}");
        s.close().await;
    }
    for sid in ["", "."] {
        let (s, _) = session(&answers(&[]), Hub::new(sid));
        let attached = UnixSocket::new(&ctl).attach(s.clone(), Grant::watch(Origin::Agent));
        assert!(
            matches!(attached, Err(TransportError::StreamId(_))),
            "{sid:?}: {attached:?}"
        );
        s.close().await;
        assert!(!ctl.join(format!("{sid}.sock")).exists(), "{sid:?}");
    }
    // A plain name, `..` and a dot in it included, still attaches.
    for sid in ["..", "s1.v2", "team-agent"] {
        let (s, _) = session(&answers(&[]), Hub::new(sid));
        let attached = UnixSocket::new(&ctl).attach(s.clone(), Grant::watch(Origin::Agent));
        assert!(attached.is_ok(), "{sid}: {:?}", attached.err());
        assert!(ctl.join(format!("{sid}.json")).exists(), "{sid}");
        drop(attached);
        s.close().await;
    }
}
