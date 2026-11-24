//! Starting the app: the tokio side, the store, the main window, the
//! approval panel and the app's own keys.

use std::{cell::RefCell, collections::HashSet, path::PathBuf, rc::Rc, time::Duration};

use gpui::{
    App, AppContext as _, Bounds, Entity, KeyBinding, Menu, MenuItem, WindowBounds, WindowHandle,
    WindowKind, WindowOptions, actions, point, px, size,
};
use gpui_component::{Root, TitleBar};

use haest::core::error::report;
use haest_gui::{
    assets::AppAssets,
    link::{self, Link},
    model::approval_queue,
    replay,
    store::Store,
    ui::{self, window::MainWindow},
};

actions!(haest_gui_app, [Quit]);

/// What the command line asked for.
#[derive(Debug, Default)]
pub(crate) struct Args {
    /// Where the control sockets are; else haest's own setting.
    pub(crate) control_dir: Option<PathBuf>,
    /// Play this recording instead of watching the control dir.
    pub(crate) replay: Option<PathBuf>,
    /// The longest pause between replayed frames.
    pub(crate) replay_pause: Duration,
}

/// The approval panel's window, opened on demand and reopened when it was closed.
struct Approvals {
    store: Entity<Store>,
    window: Option<WindowHandle<Root>>,
    /// The asks the panel was opened for (or seen while it was closed).
    seen: HashSet<String>,
}

impl Approvals {
    fn show(&mut self, focus: bool, cx: &mut App) {
        if let Some(handle) = &self.window {
            let alive = handle
                .update(cx, |_, window, _| {
                    if focus {
                        window.activate_window();
                    }
                })
                .is_ok();
            if alive {
                return;
            }
        }
        let (w, h) = (px(400.), px(480.));
        let origin = cx
            .primary_display()
            .map(|d| {
                let b = d.bounds();
                point(
                    b.origin.x + b.size.width - w - px(24.),
                    b.origin.y + px(48.),
                )
            })
            .unwrap_or_else(|| point(px(80.), px(80.)));
        let store = self.store.clone();
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin,
                    size: size(w, h),
                })),
                kind: WindowKind::PopUp,
                focus,
                show: true,
                is_minimizable: false,
                window_min_size: Some(size(px(320.), px(220.))),
                ..TitleBar::window_options()
            },
            move |window, cx| {
                let panel = ui::window::approval_panel(store, cx);
                if focus {
                    let handle = gpui::Focusable::focus_handle(panel.read(cx), cx);
                    window.focus(&handle, cx);
                }
                cx.new(|cx| Root::new(panel, window, cx))
            },
        );
        self.window = opened.ok();
    }
}

/// Open the windows and run until the main one closes.
pub(crate) fn run(args: Args) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("haest-gui-link")
        .enable_all()
        .build()?;
    let dir = runtime.block_on(link::control_dir(args.control_dir.clone()));
    let replaying = args.replay.is_some();
    let (link, events) = match &args.replay {
        Some(path) => {
            let (tx, rx) = async_channel::unbounded();
            let (path, pause) = (path.clone(), args.replay_pause);
            runtime.spawn(async move {
                if let Err(e) = replay::play(&path, tx, pause).await {
                    #[expect(clippy::print_stderr, reason = "the app's own error line")]
                    {
                        eprintln!("haest-gui: {}", report(&e));
                    }
                }
            });
            (Link::detached(), rx)
        }
        None => Link::start(runtime.handle(), dir.clone()),
    };

    gpui_platform::application()
        .with_assets(AppAssets)
        .run(move |cx| {
            gpui_component::init(cx);
            ui::init_theme(cx);
            ui::window::bind_keys(cx);
            ui::approvals::bind_keys(cx);
            cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
            cx.on_action(|_: &Quit, cx| cx.quit());
            cx.set_menus([Menu {
                name: "haest-gui".into(),
                items: vec![MenuItem::action("Quit haest-gui", Quit)],
                disabled: false,
            }]);

            let store = cx.new(|cx| {
                let mut s = Store::new(dir.clone(), link.clone());
                s.replay = replaying;
                Store::listen(events.clone(), cx);
                s
            });

            let approvals = Rc::new(RefCell::new(Approvals {
                store: store.clone(),
                window: None,
                seen: HashSet::new(),
            }));
            // A new ask shows the panel, without taking focus from anything.
            cx.observe(&store, {
                let approvals = approvals.clone();
                move |store, cx| {
                    let keys: HashSet<String> = approval_queue(&store.read(cx).sessions)
                        .iter()
                        .map(|(s, a)| format!("{}:{:?}", s.sid, a.id))
                        .collect();
                    let new = keys.iter().any(|k| !approvals.borrow().seen.contains(k));
                    approvals.borrow_mut().seen = keys;
                    if new {
                        let approvals = approvals.clone();
                        cx.defer(move |cx| approvals.borrow_mut().show(false, cx));
                    }
                }
            })
            .detach();
            let open: ui::window::OpenApprovals = {
                let approvals = approvals.clone();
                Rc::new(move |focus, cx: &mut App| {
                    let approvals = approvals.clone();
                    cx.defer(move |cx| approvals.borrow_mut().show(focus, cx));
                })
            };

            let main = cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::centered(
                        None,
                        size(px(1180.), px(780.)),
                        cx,
                    )),
                    window_min_size: Some(size(px(520.), px(380.))),
                    ..TitleBar::window_options()
                },
                |window, cx| {
                    let view = cx.new(|cx| MainWindow::new(store.clone(), open, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                },
            );
            let main_id = main.as_ref().ok().map(|w| w.window_id());
            cx.on_window_closed(move |cx, closed| {
                if Some(closed) == main_id {
                    cx.quit();
                }
            })
            .detach();
            cx.activate(true);
        });
    drop(runtime);
    Ok(())
}
