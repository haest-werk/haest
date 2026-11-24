//! Renders haest-gui's own windows offscreen to PNGs, with no display: the
//! receipt that every view looks right in every state.
//!
//! ```text
//! cargo run --release --example snapshot -- fixture fixtures/live.ndjson target/shots
//! cargo run --release --example snapshot -- live <control-dir> target/shots/live fixtures/live.ndjson
//! ```
//!
//! `fixture` plays a recording into the store, cuts it at the moments that
//! matter and shoots each view in each state. `live` attaches to the one
//! haest with a command socket in `<control-dir>` over the real link, drives
//! it the way a person in the window would (a turn, the approval answer, an
//! effort change, an interrupt, a close) and shoots as it goes; the frames it
//! saw are saved as the recording `fixture` plays.
//!
//! `terminal <control-dir> <out-dir> <tmux-target>` proves the approval panel
//! against a haest someone runs at a terminal (in tmux, so the driver can
//! type there): a turn typed at the terminal asks for approval; the panel
//! denies it (its Delete key) and the terminal's question goes; a second
//! ask is answered at the terminal, and the panel drops it.

#![expect(
    clippy::print_stdout,
    reason = "an example shows on the terminal what it does"
)]

use std::{
    cell::RefCell,
    io::Write as _,
    path::{Path, PathBuf},
    process::Command,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, bail};
use gpui::{
    Action, AnyView, AnyWindowHandle, App, AppContext as _, Bounds, Entity, Focusable as _,
    HeadlessAppContext, Window, WindowBounds, WindowOptions, point, px, size,
};
use gpui_component::Root;
use haest::core::{
    LoopEvent,
    messages::{
        AssistantMessage, AssistantPart, StopReason, ToolCall, ToolResultMessage, Usage,
        UserMessage,
    },
    tool::ToolOutput,
};
use haest::session::{Body, Frame};
use haest::wire::{
    Access, Answer, Control, Envelope, Outbound, Path as WirePath, Request, State, WIRE_V,
};
use haest_gui::{
    link::{Cmd, Event, Events, Found, Link, Tag},
    model::{AskId, Conn, ItemKind, MAIN, SessionModel, ToolStatus, approval_queue},
    replay,
    store::{Store, Tab},
    ui::{
        self,
        approvals::{ApprovalPanel, DenyAsk},
        window::MainWindow,
    },
};
use serde_json::json;

/// Pixels per point: the shots are read on a Retina screen.
const SCALE: f32 = 2.;
const MAIN_SIZE: (f32, f32) = (1180., 780.);
const NARROW_SIZE: (f32, f32) = (640., 720.);
const PANEL_SIZE: (f32, f32) = (400., 480.);

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["fixture", rec, out] => fixture(Path::new(rec), Path::new(out)),
        ["live", dir, out, rec] => live(Path::new(dir), Path::new(out), Path::new(rec)),
        ["probe", dir] => probe(Path::new(dir)),
        ["terminal", dir, out, target] => terminal(Path::new(dir), Path::new(out), target),
        _ => bail!(
            "usage: snapshot fixture <recording> <out-dir> | live <control-dir> <out-dir> <recording-out> | terminal <control-dir> <out-dir> <tmux-target>"
        ),
    }
}

// ---- the camera ----

struct Camera {
    cx: HeadlessAppContext,
    out: PathBuf,
}

impl Camera {
    fn new(out: &Path) -> Result<Self> {
        std::fs::create_dir_all(out)?;
        let mut cx = HeadlessAppContext::with_platform(
            gpui_platform::current_platform(true).text_system(),
            Arc::new(haest_gui::assets::AppAssets),
            gpui_platform::current_headless_renderer,
        );
        cx.update(|cx| {
            gpui_component::init(cx);
            ui::init_theme(cx);
            ui::window::bind_keys(cx);
            ui::approvals::bind_keys(cx);
        });
        Ok(Self {
            cx,
            out: out.into(),
        })
    }

    fn store(&mut self, link: Link, replay: bool) -> Entity<Store> {
        self.cx.update(|cx| {
            cx.new(|_| {
                let mut s = Store::new(PathBuf::from("~/.local/share/haest/control"), link);
                s.replay = replay;
                s
            })
        })
    }

    fn change(&mut self, store: &Entity<Store>, f: impl FnOnce(&mut Store)) {
        self.cx.update(|cx| {
            store.update(cx, |s, cx| {
                f(s);
                cx.notify();
            })
        });
    }

    fn read<R>(&mut self, store: &Entity<Store>, f: impl FnOnce(&Store) -> R) -> R {
        self.cx.update(|cx| f(store.read(cx)))
    }

    fn open(
        &mut self,
        (w, h): (f32, f32),
        build: impl FnOnce(&mut Window, &mut App) -> AnyView,
    ) -> Result<AnyWindowHandle> {
        let handle = self.cx.update(|cx| {
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: point(px(0.), px(0.)),
                        size: size(px(w), px(h)),
                    })),
                    focus: false,
                    show: false,
                    ..Default::default()
                },
                |window, cx| {
                    let view = build(window, cx);
                    cx.new(|cx| Root::new(view, window, cx))
                },
            )
        })?;
        let any: AnyWindowHandle = handle.into();
        self.cx.simulate_window_scale_factor_change(any, SCALE);
        Ok(any)
    }

    fn main_window(&mut self, store: &Entity<Store>, size: (f32, f32)) -> Result<AnyWindowHandle> {
        let store = store.clone();
        self.open(size, move |window, cx| {
            let open: ui::window::OpenApprovals = Rc::new(|_, _| {});
            cx.new(|cx| MainWindow::new(store, open, window, cx)).into()
        })
    }

    fn panel_window(&mut self, store: &Entity<Store>) -> Result<AnyWindowHandle> {
        let store = store.clone();
        self.open(PANEL_SIZE, move |_, cx| {
            ui::window::approval_panel(store, cx).into()
        })
    }

    /// Draw until the layout settles (lists measure their rows a frame
    /// late), then save the window as `<name>.png`.
    fn shoot(&mut self, window: AnyWindowHandle, name: &str) -> Result<()> {
        for _ in 0..5 {
            self.cx.run_until_parked();
            self.cx.update_window(window, |_, window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            })?;
        }
        let image = self.cx.capture_screenshot(window)?;
        let path = self.out.join(format!("{name}.png"));
        image
            .save(&path)
            .with_context(|| path.display().to_string())?;
        println!("shot {}", path.display());
        Ok(())
    }

    fn close(&mut self, window: AnyWindowHandle) {
        let _ = self
            .cx
            .update_window(window, |_, window, _| window.remove_window());
        self.cx.run_until_parked();
    }

    /// A fresh main window of `size` on `store`, shot as `name`, closed again.
    fn shoot_main(&mut self, store: &Entity<Store>, size: (f32, f32), name: &str) -> Result<()> {
        let w = self.main_window(store, size)?;
        self.shoot(w, name)?;
        self.close(w);
        Ok(())
    }

    fn shoot_panel(&mut self, store: &Entity<Store>, name: &str) -> Result<()> {
        let w = self.panel_window(store)?;
        self.shoot(w, name)?;
        self.close(w);
        Ok(())
    }

    /// The approval panel on `store`, left open: its window and its view.
    fn open_panel(
        &mut self,
        store: &Entity<Store>,
    ) -> Result<(AnyWindowHandle, Entity<ApprovalPanel>)> {
        let store = store.clone();
        let made = Rc::new(RefCell::new(None));
        let keep = made.clone();
        let w = self.open(PANEL_SIZE, move |_, cx| {
            let panel = ui::window::approval_panel(store, cx);
            *keep.borrow_mut() = Some(panel.clone());
            panel.into()
        })?;
        let panel = made
            .borrow_mut()
            .take()
            .context("the panel was not built")?;
        Ok((w, panel))
    }

    /// The human clicks into the panel: it has the keyboard from now on.
    fn focus_panel(&mut self, w: AnyWindowHandle, panel: &Entity<ApprovalPanel>) -> Result<()> {
        let panel = panel.clone();
        self.cx.update_window(w, |_, window, cx| {
            let focus = panel.read(cx).focus_handle(cx);
            window.focus(&focus, cx);
        })?;
        self.cx.run_until_parked();
        self.cx.update_window(w, |_, window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        })?;
        Ok(())
    }

    /// A key on the focused panel: the action its binding dispatches.
    fn press(&mut self, w: AnyWindowHandle, action: Box<dyn Action>) -> Result<()> {
        self.cx
            .update_window(w, |_, window, cx| window.dispatch_action(action, cx))?;
        self.cx.run_until_parked();
        Ok(())
    }
}

fn session<'a>(s: &'a Store, sid: &str) -> &'a SessionModel {
    s.session(sid).expect("attached")
}

/// The first subagent of `sid`, if any started.
fn first_subagent(s: &Store, sid: &str) -> Option<String> {
    s.session(sid)?
        .tree
        .flatten()
        .into_iter()
        .find(|(depth, _)| *depth > 0)
        .map(|(_, n)| n.id.clone())
}

// ---- fixture: every state from a recording ----

fn fixture(rec: &Path, out: &Path) -> Result<()> {
    let text = std::fs::read_to_string(rec).with_context(|| rec.display().to_string())?;
    let frames = replay::parse(&text);
    let found = replay::found(&frames, Access::Command).context("the recording has no frames")?;
    let sid = found.sid.clone();
    let mut cam = Camera::new(out)?;

    // The moments that matter: the first approval ask, and a subagent running.
    let ask_at = frames
        .iter()
        .position(|f| matches!(f.body, Body::Wire(Outbound::ApprovalAsk { .. })))
        .unwrap_or(frames.len() / 2);
    let agent_at = frames
        .iter()
        .position(|f| matches!(f.body, Body::Wire(Outbound::SubagentStarted { .. })))
        .map(|i| (i + 4).min(frames.len() - 1))
        .unwrap_or(ask_at);

    // No session: looking, none running, some to pick.
    let store = cam.store(Link::detached(), true);
    cam.shoot_main(&store, MAIN_SIZE, "01-looking")?;
    cam.change(&store, |s| s.found = Some(Vec::new()));
    cam.shoot_main(&store, MAIN_SIZE, "02-no-haest")?;
    let other = Found {
        sid: "7f3c2a9104be".into(),
        session: String::new(),
        model: "openrouter:anthropic/claude-sonnet-5.5".into(),
        pid: 0,
        access: Access::Watch,
        started_ms: found.started_ms.saturating_sub(3_600_000),
    };
    cam.change(&store, |s| {
        s.found = Some(vec![other.clone(), found.clone()])
    });
    cam.shoot_main(&store, MAIN_SIZE, "03-pick")?;
    cam.change(&store, |s| s.attach(&sid));
    cam.shoot_main(&store, MAIN_SIZE, "04-connecting")?;
    cam.shoot_main(&store, NARROW_SIZE, "05-narrow-no-sidebar")?;

    // Mid-turn, at the approval ask.
    let store = played(&mut cam, &frames[..=ask_at], Access::Command);
    cam.change(&store, |s| {
        s.found = Some(vec![other.clone(), found.clone()])
    });
    cam.shoot_main(&store, MAIN_SIZE, "10-stream-mid-turn")?;
    cam.shoot_panel(&store, "30-approvals-one")?;
    cam.change(&store, |s| s.tab = Tab::Timeline);
    cam.shoot_main(&store, MAIN_SIZE, "20-timeline-running")?;

    // A subagent at work.
    let store = played(&mut cam, &frames[..=agent_at], Access::Command);
    let agent = cam.read(&store, |s| first_subagent(s, &sid));
    cam.change(&store, |s| {
        s.tab = Tab::Agents;
        if let Some(a) = &agent {
            s.select_agent(&sid, a);
        }
    });
    cam.shoot_main(&store, MAIN_SIZE, "25-agents-running")?;

    // The whole recording.
    let store = played(&mut cam, &frames, Access::Command);
    cam.change(&store, |s| {
        s.found = Some(vec![other.clone(), found.clone()])
    });
    cam.shoot_main(&store, MAIN_SIZE, "11-stream-done")?;
    cam.shoot_main(&store, NARROW_SIZE, "12-stream-narrow")?;
    cam.change(&store, |s| s.tab = Tab::Timeline);
    cam.shoot_main(&store, MAIN_SIZE, "21-timeline")?;
    cam.shoot_main(&store, NARROW_SIZE, "22-timeline-narrow")?;
    cam.change(&store, |s| {
        s.tab = Tab::Agents;
        if let Some(a) = &agent {
            s.select_agent(&sid, a);
        }
    });
    cam.shoot_main(&store, MAIN_SIZE, "26-agents-done")?;
    cam.shoot_main(&store, NARROW_SIZE, "27-agents-narrow")?;
    cam.shoot_panel(&store, "31-approvals-empty")?;

    // Watching only: no composer, no controls.
    let store = played(&mut cam, &frames, Access::Watch);
    cam.shoot_main(&store, MAIN_SIZE, "13-stream-watch")?;

    // The socket gone mid-turn, while discovery still lists it.
    let store = played(&mut cam, &frames[..=ask_at], Access::Command);
    cam.change(&store, |s| {
        s.found = Some(vec![found.clone()]);
        s.apply(Event::Lost {
            sid: sid.clone(),
            why: "the control socket closed".into(),
        });
    });
    cam.shoot_main(&store, MAIN_SIZE, "14-stream-lost")?;
    cam.shoot_panel(&store, "34-approvals-after-lost")?;

    // Long content: a long tool result and a long answer, appended.
    let store = played(&mut cam, &frames, Access::Command);
    let last = frames.last().map_or((0, 0), |f| (f.seq, f.ts));
    for frame in long_turn(&sid, last) {
        cam.change(&store, |s| {
            s.apply(Event::Frame {
                sid: sid.clone(),
                frame: Box::new(frame),
            })
        });
    }
    cam.shoot_main(&store, MAIN_SIZE, "15-stream-long")?;
    cam.change(&store, |s| s.tab = Tab::Timeline);
    cam.shoot_main(&store, MAIN_SIZE, "23-timeline-long")?;

    // The approval queue across three haests: one answerable, one sent, one
    // asked at a terminal, one on a watch socket.
    let store = played(&mut cam, &frames[..=ask_at], Access::Command);
    let t0 = frames[ask_at].ts;
    cam.change(&store, |s| {
        s.sessions.push(extra_session("2b8e41d07c55", Access::Command, "openrouter:anthropic/claude-sonnet-5.5", t0 + 1_500, vec![
            Outbound::ApprovalAsk {
                ask: 1,
                tool_name: "Bash".into(),
                tool_call_id: Some("toolu_01".into()),
                description: "Delete recursively".into(),
                preview: "rm -rf target/debug/incremental".into(),
                key: Some("rm -rf target/debug/incremental".into()),
            },
            // The same ask on its own terminal: one ask, either side answers.
            Outbound::Ask {
                text: "Allow Bash: rm -rf target/debug/incremental?".into(),
                approval: Some(1),
            },
            Outbound::ApprovalAsk {
                ask: 2,
                tool_name: "Bash".into(),
                tool_call_id: Some("toolu_02".into()),
                description: "Rewrite git history".into(),
                preview: "git push --force-with-lease origin feat/acp\n# rewrites 3 commits on origin".into(),
                key: None,
            },
        ]));
        // A haest whose socket only watches asks at its terminal alone.
        s.sessions.push(extra_session("91d4ee3a6f20", Access::Watch, "anthropic:claude-opus-5-5", t0 + 2_500, vec![
            Outbound::Ask {
                text: "Allow Bash: sudo launchctl kickstart -k system/com.example.agent?".into(),
                approval: None,
            },
        ]));
        s.sessions.push(extra_session("c03f6b5e1a88", Access::Watch, "openai:gpt-6.1-sol", t0 + 4_000, vec![
            Outbound::ApprovalAsk {
                ask: 7,
                tool_name: "Bash".into(),
                tool_call_id: None,
                description: "Kill processes by name".into(),
                preview: "pkill -f vite".into(),
                key: Some("pkill -f vite".into()),
            },
        ]));
    });
    cam.shoot_panel(&store, "32-approvals-queue")?;
    cam.change(&store, |s| {
        s.replay = false;
        if let Some(x) = s.sessions.iter_mut().find(|x| x.sid == "2b8e41d07c55") {
            x.answering(AskId::Approval(1), Answer::Allow);
        }
    });
    cam.shoot_panel(&store, "33-approvals-sent")?;
    cam.shoot_main(&store, MAIN_SIZE, "16-main-with-asks")?;
    Ok(())
}

/// A store with `frames` applied, as a replay would.
fn played(cam: &mut Camera, frames: &[Frame], access: Access) -> Entity<Store> {
    let store = cam.store(Link::detached(), true);
    cam.change(&store, |s| {
        for ev in replay::events(frames.to_vec(), access) {
            s.apply(ev);
        }
    });
    store
}

/// Another attached haest, live, whose stream is `asks` on main.
fn extra_session(
    sid: &str,
    access: Access,
    model: &str,
    ts: u64,
    asks: Vec<Outbound>,
) -> SessionModel {
    let mut s = SessionModel::new(sid, access);
    s.conn = Conn::Live;
    s.model = model.into();
    s.state = State::Running;
    for (i, body) in asks.into_iter().enumerate() {
        let seq = s.last_seq + 1;
        s.apply(&frame(
            sid,
            seq,
            ts + i as u64 * 700,
            WirePath::main(),
            body,
        ));
    }
    s
}

fn frame(sid: &str, seq: u64, ts: u64, path: WirePath, body: impl Into<Body>) -> Frame {
    Envelope {
        v: WIRE_V,
        sid: sid.into(),
        seq,
        ts,
        path,
        body: body.into(),
    }
}

/// A turn with a long tool result and a long markdown answer, after `(seq, ts)`.
fn long_turn(sid: &str, (seq, ts): (u64, u64)) -> Vec<Frame> {
    let log: String = (1..=240)
        .map(|i| {
            format!(
                "   Compiling crate-{i:03} v0.{}.{} (registry)\n",
                i % 7,
                i % 13
            )
        })
        .collect();
    let call = ToolCall {
        id: "long-1".into(),
        name: "Bash".into(),
        input: json!({"command": "cargo build --release 2>&1 | tee build.log", "description": "Build everything and keep the log"})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    };
    let mut ask = AssistantMessage::start("openrouter", "anthropic/claude-haiku-5.5");
    ask.content = vec![AssistantPart::ToolCall(call.clone())];
    ask.stop_reason = StopReason::ToolUse;
    ask.usage = Usage {
        input: 220,
        output: 64,
        cache_read: 5_400,
        cache_write: 300,
        ..Usage::default()
    };
    let mut output = ToolOutput::text(&log);
    output.is_error = false;
    let result = LoopEvent::ToolResult {
        call: call.clone(),
        message: ToolResultMessage {
            tool_call_id: call.id.clone(),
            tool_name: call.name.clone(),
            content: log.clone(),
            is_error: false,
            media: Vec::new(),
        },
        output,
        duration_ms: 41_200,
    };
    let answer_text = "## Build finished\n\n\
        The release build compiled **240 crates** in 41 s with no warnings. \
        The slowest were the GPU shaders and the TLS stack, which is normal on a cold cache.\n\n\
        ### What changed since the last build\n\n\
        - `haest-wire` moved to 0.6.0, so every crate that links it rebuilt.\n\
        - `gpui-pre` kept its version; its 80 crates came from the cache.\n\
        - The binary grew by 1.2 MB, almost all of it the new ACP transport.\n\n\
        ### Next\n\n\
        1. Run the test suite with `cargo test --profile fast`.\n\
        2. Ship the image with `deploy/rust/build.sh`.\n\n\
        ```sh\ncargo test --profile fast 2>&1 | tee test.log | tail -5\n```\n\n\
        | Stage | Time |\n|---|---|\n| Resolve | 0.4 s |\n| Compile | 38.9 s |\n| Link | 1.9 s |\n";
    let mut done = AssistantMessage::start("openrouter", "anthropic/claude-haiku-5.5");
    done.content = vec![AssistantPart::Text {
        text: answer_text.into(),
    }];
    done.usage = Usage {
        input: 410,
        output: 260,
        cache_read: 5_700,
        cache_write: 0,
        ..Usage::default()
    };
    let events: Vec<(u64, LoopEvent)> = vec![
        (
            0,
            LoopEvent::User {
                message: UserMessage {
                    content: "Build the release binary and tell me how it went.".into(),
                    media: Vec::new(),
                },
            },
        ),
        (
            100,
            LoopEvent::ModelStart {
                provider: "openrouter".into(),
                model: "anthropic/claude-haiku-5.5".into(),
            },
        ),
        (1_900, LoopEvent::Assistant { message: ask }),
        (43_100, result),
        (
            43_200,
            LoopEvent::ModelStart {
                provider: "openrouter".into(),
                model: "anthropic/claude-haiku-5.5".into(),
            },
        ),
        (47_800, LoopEvent::Assistant { message: done }),
        (
            47_850,
            LoopEvent::TurnEnd {
                stop_reason: StopReason::EndTurn,
                usage: Usage {
                    input: 630,
                    output: 324,
                    cache_read: 11_100,
                    cache_write: 300,
                    cost_usd: Some(0.0004),
                    ..Usage::default()
                },
                duration_ms: 47_850,
                error: None,
            },
        ),
    ];
    events
        .into_iter()
        .enumerate()
        .map(|(i, (dt, ev))| {
            frame(
                sid,
                seq + 1 + i as u64,
                ts + 5_000 + dt,
                WirePath::main(),
                ev,
            )
        })
        .collect()
}

// ---- live: drive a real haest over the real link ----

// The commands print nothing about the machine or who runs it (no owner,
// host, home path or clock): the recording ships with the crate.
const TURN_1: &str = "Do three steps. Step 1: in ONE message, call Bash three times in parallel: `ls`, `wc -l notes.txt`, `echo ok`. \
Step 2: start one subagent with the Agent tool; tell it to use Read on notes.txt and reply with the file's first line. \
Step 3: run `rm -rf scratch` with Bash. \
Then answer in short markdown: a heading, then one bullet per step.";
const TURN_2: &str = "Run `sleep 30` with Bash, then say done.";
const TURN_3: &str = "Run `sleep 20` with Bash, then say done.";

/// The frames the link handed over, kept for the recording.
struct Live {
    cam: Camera,
    store: Entity<Store>,
    events: Events,
    recorded: Vec<String>,
}

impl Live {
    /// Apply what arrived until `done` holds, or fail after `limit`.
    fn pump(&mut self, what: &str, limit: Duration, done: impl Fn(&Store) -> bool) -> Result<()> {
        let start = Instant::now();
        loop {
            while let Ok(ev) = self.events.try_recv() {
                match &ev {
                    Event::Sessions(found) => println!("found {} haest(s)", found.len()),
                    Event::Lost { why, .. } => println!("lost: {why}"),
                    Event::Note { text, .. } => println!("note: {text}"),
                    _ => {}
                }
                if let Event::Frame { frame, .. } = &ev {
                    self.recorded.push(serde_json::to_string(frame)?);
                }
                if let Event::Reply {
                    tag,
                    result: Err(why),
                    ..
                } = &ev
                {
                    println!("reply to {tag:?} failed: {why}");
                }
                let store = self.store.clone();
                self.cam.change(&store, |s| s.apply(ev));
            }
            let store = self.store.clone();
            if self.cam.read(&store, &done) {
                println!("ok: {what} ({:.1} s)", start.elapsed().as_secs_f32());
                return Ok(());
            }
            if start.elapsed() > limit {
                bail!("timed out waiting for: {what}");
            }
            std::thread::sleep(Duration::from_millis(40));
        }
    }

    fn change(&mut self, f: impl FnOnce(&mut Store)) {
        let store = self.store.clone();
        self.cam.change(&store, f);
    }

    fn shoot_main(&mut self, name: &str) -> Result<()> {
        let store = self.store.clone();
        self.cam.shoot_main(&store, MAIN_SIZE, name)
    }

    fn shoot_panel(&mut self, name: &str) -> Result<()> {
        let store = self.store.clone();
        self.cam.shoot_panel(&store, name)
    }
}

fn turn_over(s: &Store, sid: &str, turns: usize) -> bool {
    s.session(sid).is_some_and(|x| {
        x.timelines
            .get(MAIN)
            .is_some_and(|t| t.turns.len() >= turns && !t.is_running())
    })
}

fn bash_running(s: &Store, sid: &str) -> bool {
    s.session(sid).and_then(|x| x.stream(MAIN)).is_some_and(|st| {
        st.items.iter().any(|i| {
            matches!(&i.kind, ItemKind::Tool(c) if c.name == "Bash" && c.status == ToolStatus::Running && c.input.is_some())
        })
    })
}

/// What discovery finds in `dir`, through the link, within 10 s.
fn probe(dir: &Path) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let (_link, events) = Link::start(runtime.handle(), dir.to_path_buf());
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        if let Ok(Event::Sessions(found)) = events.try_recv() {
            println!(
                "found {}: {:?}",
                found.len(),
                found.iter().map(|f| &f.sid).collect::<Vec<_>>()
            );
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    bail!("nothing from discovery in 10 s")
}

fn live(dir: &Path, out: &Path, rec: &Path) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let (link, events) = Link::start(runtime.handle(), dir.to_path_buf());
    let mut cam = Camera::new(out)?;
    let store = cam.store(link.clone(), false);
    let mut live = Live {
        cam,
        store,
        events,
        recorded: Vec::new(),
    };
    let minute = Duration::from_secs(60);

    live.pump("a haest with a command socket", minute, |s| {
        s.found
            .iter()
            .flatten()
            .any(|f| f.access == Access::Command)
    })?;
    let found = live.cam.read(&live.store.clone(), |s| {
        s.found
            .iter()
            .flatten()
            .find(|f| f.access == Access::Command)
            .cloned()
    });
    let found = found.context("no command socket")?;
    let sid = found.sid.clone();
    println!("attaching to {} (pid {}, {})", sid, found.pid, found.model);
    live.shoot_main("live-01-pick")?;
    live.change(|s| s.attach(&sid));
    live.pump("attached", minute, |s| session(s, &sid).conn == Conn::Live)?;
    live.shoot_main("live-02-attached")?;

    // Turn 1: parallel tools, a subagent, an approval ask.
    let mut ok = false;
    live.change(|s| ok = s.send_turn(TURN_1.into()));
    if !ok {
        bail!("the store refused the turn");
    }
    live.pump("the approval ask (or the turn's end)", 3 * minute, |s| {
        !approval_queue(&s.sessions).is_empty() || turn_over(s, &sid, 1)
    })?;
    let ask = live.cam.read(&live.store.clone(), |s| {
        approval_queue(&s.sessions).first().map(|(_, a)| a.id)
    });
    if let Some(ask) = ask {
        // Let the batch's other calls land before the shots.
        std::thread::sleep(Duration::from_millis(800));
        live.pump("frames so far", minute, |_| true)?;
        live.shoot_main("live-03-mid-turn")?;
        live.shoot_panel("live-04-approval")?;
        let agent = live
            .cam
            .read(&live.store.clone(), |s| first_subagent(s, &sid));
        if let Some(a) = agent {
            live.change(|s| {
                s.tab = Tab::Agents;
                s.select_agent(&sid, &a);
            });
            live.shoot_main("live-05-agents-mid-turn")?;
            live.change(|s| s.tab = Tab::Stream);
        }
        // Deny: the haest must not delete anything.
        println!("answering {ask:?}: deny");
        live.change(|s| s.answer(&sid, ask, Answer::Deny));
        live.shoot_panel("live-06-approval-sent")?;
        live.pump("the ask leaves the queue", minute, |s| {
            approval_queue(&s.sessions).is_empty()
        })?;
    } else {
        println!("NOTE: turn 1 ended with no approval ask");
    }
    live.pump("turn 1 ends", 3 * minute, |s| turn_over(s, &sid, 1))?;
    live.shoot_main("live-07-turn-done")?;
    live.change(|s| s.tab = Tab::Timeline);
    live.shoot_main("live-08-timeline")?;
    let agent = live
        .cam
        .read(&live.store.clone(), |s| first_subagent(s, &sid));
    live.change(|s| {
        s.tab = Tab::Agents;
        if let Some(a) = &agent {
            s.select_agent(&sid, a);
        }
    });
    live.shoot_main("live-09-agents")?;
    live.change(|s| s.tab = Tab::Stream);

    // The effort picker's path: a control, confirmed by its reply.
    live.change(|s| s.set_effort(Some("medium".into())));
    live.pump("effort is medium", minute, |s| {
        session(s, &sid).effort.as_deref() == Some("medium")
    })?;

    // Turn 2: interrupted while its Bash call runs.
    live.change(|s| ok = s.send_turn(TURN_2.into()));
    live.pump("turn 2's Bash call runs", 2 * minute, |s| {
        bash_running(s, &sid)
            && session(s, &sid)
                .timelines
                .get(MAIN)
                .is_some_and(|t| t.turns.len() >= 2)
    })?;
    live.shoot_main("live-10-running")?;
    live.change(|s| s.interrupt());
    live.pump("turn 2 ends, interrupted", minute, |s| {
        turn_over(s, &sid, 2)
    })?;
    live.change(|s| s.tab = Tab::Timeline);
    live.shoot_main("live-11-timeline-two-turns")?;
    live.change(|s| s.tab = Tab::Stream);

    // Turn 3: the haest closes while its Bash call runs: the socket goes
    // mid-stream.
    live.change(|s| ok = s.send_turn(TURN_3.into()));
    live.pump("turn 3's Bash call runs", 2 * minute, |s| {
        bash_running(s, &sid)
            && session(s, &sid)
                .timelines
                .get(MAIN)
                .is_some_and(|t| t.turns.len() >= 3)
    })?;
    link.send(Cmd::Send {
        sid: sid.clone(),
        request: Request::Control(Control::Close),
        tag: Tag::Interrupt,
    });
    live.pump("the connection is lost", minute, |s| {
        matches!(session(s, &sid).conn, Conn::Lost(_))
    })?;
    live.shoot_main("live-12-lost")?;
    live.pump("discovery drops it", minute, |s| !s.is_listed(&sid))?;
    live.shoot_main("live-13-ended")?;

    if let Some(parent) = rec.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = std::fs::File::create(rec)?;
    for line in &live.recorded {
        writeln!(f, "{line}")?;
    }
    println!(
        "recorded {} frames to {}",
        live.recorded.len(),
        rec.display()
    );
    let s = live.cam.read(&live.store.clone(), |s| {
        let x = session(s, &sid);
        let t = x.timelines.get(MAIN);
        (
            t.map_or(0, |t| t.turns.len()),
            t.map_or(0, |t| t.turns.iter().map(|t| t.calls.len()).sum::<usize>()),
            t.map_or(0, |t| t.turns.iter().map(|t| t.tools.len()).sum::<usize>()),
            x.tree.subagents(),
            x.usd,
        )
    });
    println!(
        "turns {}, model calls {}, tool calls {}, subagents {}, usd {:?}",
        s.0, s.1, s.2, s.3, s.4
    );
    drop(runtime);
    Ok(())
}

// ---- terminal: the panel against a haest at a terminal (tmux) ----

const PROOF_1: &str = "Run `rm -rf scratch` with Bash, then say done in one short sentence.";
const PROOF_2: &str = "Run `rm -rf build-cache` with Bash, then say done in one short sentence.";
const QUESTION: &str = "[y]es / [a]lways / [N]o";
const AT_THE_PROMPT: &str = "press Enter at the prompt to answer it";

/// `tmux <args>`: its stdout. `SNAPSHOT_TMUX_L` names the tmux server's
/// socket (`tmux -L`), when the haest runs on a server of its own.
fn tmux(args: &[&str]) -> Result<String> {
    let mut cmd = Command::new("tmux");
    if let Ok(name) = std::env::var("SNAPSHOT_TMUX_L") {
        cmd.args(["-L", &name]);
    }
    let out = cmd.args(args).output()?;
    if !out.status.success() {
        bail!("tmux {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// What the terminal shows, its history included, wrapped lines joined.
fn pane(target: &str) -> Result<String> {
    tmux(&["capture-pane", "-p", "-J", "-S", "-2000", "-t", target])
}

/// The human types `text` at the terminal and presses Enter.
fn type_line(target: &str, text: &str) -> Result<()> {
    tmux(&["send-keys", "-t", target, "-l", text])?;
    std::thread::sleep(Duration::from_millis(150));
    tmux(&["send-keys", "-t", target, "Enter"])?;
    Ok(())
}

/// Waits up to `limit` for the terminal to show what `done` looks for.
fn wait_pane(
    target: &str,
    what: &str,
    limit: Duration,
    done: impl Fn(&str) -> bool,
) -> Result<String> {
    let start = Instant::now();
    loop {
        let shown = pane(target)?;
        if done(&shown) {
            println!(
                "ok: terminal {what} ({:.1} s)",
                start.elapsed().as_secs_f32()
            );
            return Ok(shown);
        }
        if start.elapsed() > limit {
            bail!("timed out waiting for the terminal: {what}\n{shown}");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The approval ask haest-gui shows for `sid`, the oldest.
fn first_ask(s: &Store, sid: &str) -> Option<AskId> {
    approval_queue(&s.sessions)
        .into_iter()
        .find(|(x, _)| x.sid == sid)
        .map(|(_, a)| a.id)
}

/// The terminal draws its question for the new ask: at once, or (its REPL
/// prompt holds the keyboard) once Enter is pressed there, as it says.
fn terminal_asks(target: &str, questions_before: usize, prompts_before: usize) -> Result<String> {
    let shown = wait_pane(target, "asks", Duration::from_secs(60), |p| {
        p.matches(QUESTION).count() > questions_before
            || p.matches(AT_THE_PROMPT).count() > prompts_before
    })?;
    if shown.matches(QUESTION).count() > questions_before {
        return Ok(shown);
    }
    println!("the prompt holds the keyboard: Enter, as the terminal says");
    tmux(&["send-keys", "-t", target, "Enter"])?;
    wait_pane(target, "draws the question", Duration::from_secs(30), |p| {
        p.matches(QUESTION).count() > questions_before
    })
}

fn terminal(dir: &Path, out: &Path, target: &str) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let (link, events) = Link::start(runtime.handle(), dir.to_path_buf());
    let mut cam = Camera::new(out)?;
    let store = cam.store(link, false);
    let mut live = Live {
        cam,
        store,
        events,
        recorded: Vec::new(),
    };
    let minute = Duration::from_secs(60);
    live.pump("a haest with a command socket", minute, |s| {
        s.found
            .iter()
            .flatten()
            .any(|f| f.access == Access::Command)
    })?;
    let sid = live
        .cam
        .read(&live.store.clone(), |s| {
            s.found
                .iter()
                .flatten()
                .find(|f| f.access == Access::Command)
                .map(|f| f.sid.clone())
        })
        .context("no command socket")?;
    live.change(|s| s.attach(&sid));
    live.pump("attached", minute, |s| session(s, &sid).conn == Conn::Live)?;

    // Proof 1: asked at the terminal and in the panel; the panel denies it.
    let shown = pane(target)?;
    let (q0, p0) = (
        shown.matches(QUESTION).count(),
        shown.matches(AT_THE_PROMPT).count(),
    );
    type_line(target, PROOF_1)?;
    live.pump("the approval ask", 2 * minute, |s| {
        first_ask(s, &sid).is_some()
    })?;
    terminal_asks(target, q0, p0)?;
    live.pump("the terminal's side of the ask", minute, |s| {
        session(s, &sid).asks.iter().any(|a| a.at_terminal)
    })?;
    live.shoot_main("live-14-terminal-ask")?;
    let store = live.store.clone();
    let (w, panel) = live.cam.open_panel(&store)?;
    // A new ask ignores keys for its first moments (ARM_MS).
    std::thread::sleep(Duration::from_millis(ui::approvals::ARM_MS + 200));
    live.cam.focus_panel(w, &panel)?;
    live.cam.shoot(w, "live-15-terminal-ask-panel")?;
    println!("the panel's Delete key: deny");
    live.cam.press(w, DenyAsk.boxed_clone())?;
    live.pump("the ask leaves the panel", minute, |s| {
        first_ask(s, &sid).is_none()
    })?;
    live.cam.shoot(w, "live-16-denied-from-the-panel")?;
    let shown = wait_pane(target, "says another program denied it", minute, |p| {
        p.contains("Another program denied it.")
    })?;
    std::fs::write(out.join("live-16-terminal.txt"), &shown)?;
    live.pump("turn 1 goes on to its end", 2 * minute, |s| {
        turn_over(s, &sid, 1)
    })?;
    live.shoot_main("live-17-after-the-panel-denied")?;

    // Proof 2: asked again; the human answers at the terminal.
    std::thread::sleep(Duration::from_millis(500));
    let shown = pane(target)?;
    let (q1, p1) = (
        shown.matches(QUESTION).count(),
        shown.matches(AT_THE_PROMPT).count(),
    );
    type_line(target, PROOF_2)?;
    live.pump("the second approval ask", 2 * minute, |s| {
        first_ask(s, &sid).is_some()
    })?;
    terminal_asks(target, q1, p1)?;
    live.pump("the terminal's side of it", minute, |s| {
        session(s, &sid).asks.iter().any(|a| a.at_terminal)
    })?;
    live.cam.shoot(w, "live-18-terminal-ask-2")?;
    println!("the human types n at the terminal");
    let typed = Instant::now();
    type_line(target, "n")?;
    live.pump("the panel drops the ask", minute, |s| {
        first_ask(s, &sid).is_none()
    })?;
    println!(
        "dropped {:.2} s after the answer was typed",
        typed.elapsed().as_secs_f32()
    );
    live.cam.shoot(w, "live-19-answered-at-terminal")?;
    live.pump("turn 2 goes on to its end", 2 * minute, |s| {
        turn_over(s, &sid, 2)
    })?;
    live.shoot_main("live-20-after-terminal-denied")?;
    std::fs::write(out.join("live-20-terminal.txt"), pane(target)?)?;
    live.cam.close(w);
    let usd = live.cam.read(&live.store.clone(), |s| session(s, &sid).usd);
    println!("usd {usd:?}");
    drop(runtime);
    Ok(())
}
