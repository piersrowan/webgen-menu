// webgen-menu -- WebGen Linux start menu. A layer-shell popup anchored bottom-left (where the
// yambar "Applications" button lives): a branded accent rail, category rows, and a flyout of apps
// per category. Replaces fuzzel on the panel button -- stable order (no frecency shuffle), icons +
// labels, click to launch. Categories are derived from each app's .desktop `Categories`, bucketed
// with a fixed priority so ordering is deterministic. See the design mockup approved 2026-07-24.
use adw::Application;
use gtk::gdk::Display;
use gtk::prelude::*;
use gtk::{glib, Align, ApplicationWindow, Box as GtkBox, CssProvider, EventControllerKey,
          EventControllerMotion, GestureClick, Image, Label, ListBox, ListBoxRow, Orientation,
          Popover, PositionType, SelectionMode, Separator};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use std::cell::RefCell;
use std::process::Command;
use std::rc::Rc;
use std::time::{Duration, Instant};

const APP_ID: &str = "com.webgen.Menu";

// Category buckets, in display order, with icon names that ACTUALLY exist in the shipped Adwaita
// theme (it has the -symbolic variants; there is no plain "applications-internet", so Internet uses
// the web-browser glyph). Buckets are kept in step with the wgpkg groups (see docs/wgpkg-groups.md).
const BUCKETS: &[(&str, &str)] = &[
    ("Internet", "web-browser-symbolic"),
    ("Sound & Video", "applications-multimedia-symbolic"),
    ("Graphics", "applications-graphics-symbolic"),
    ("Documents", "x-office-document-symbolic"),
    ("Utilities", "applications-utilities-symbolic"),
    ("System", "applications-system-symbolic"),
    ("Games", "applications-games-symbolic"),
];

// Resolve a .desktop Icon= name against the theme, falling back to a generic app glyph so a menu
// entry never renders blank (some apps ship only hicolor PNGs / no icon at all).
fn resolve_icon(name: &str) -> String {
    if !name.is_empty() {
        if let Some(d) = Display::default() {
            if gtk::IconTheme::for_display(&d).has_icon(name) {
                return name.to_string();
            }
        }
    }
    "application-x-executable-symbolic".to_string()
}

#[derive(Clone)]
struct AppEntry {
    name: String,
    exec: String,
    terminal: bool,
    icon: String,
    bucket: &'static str,
}

// Fixed-priority bucketing that mirrors the approved mockup grouping.
fn bucket_for(categories: &str) -> &'static str {
    let has = |c: &str| categories.split(';').any(|x| x.trim().eq_ignore_ascii_case(c));
    if has("Game") {
        "Games"
    } else if has("WebBrowser") || has("Network") {
        "Internet"
    } else if has("AudioVideo") || has("Audio") || has("Video") || has("Player") {
        "Sound & Video"
    } else if has("Graphics") || has("2DGraphics") || has("RasterGraphics") {
        "Graphics"
    } else if has("Office") || has("Viewer") {
        "Documents"
    } else if has("Settings") {
        "System"
    } else if has("TerminalEmulator") || has("Monitor") {
        "System"
    } else if has("FileManager") || has("TextEditor") || has("Development") || has("Utility") {
        "Utilities"
    } else if has("System") {
        "System"
    } else {
        "Utilities"
    }
}

// Split Exec into argv and drop the %f/%F/%u/%U/%i/%c/%k field codes. Our own .desktop files
// don't quote arguments, so whitespace splitting is sufficient.
fn exec_argv(exec: &str) -> Vec<String> {
    exec.split_whitespace()
        .filter(|t| !(t.len() == 2 && t.starts_with('%')))
        .map(str::to_string)
        .collect()
}

fn load_apps() -> Vec<AppEntry> {
    let mut dirs: Vec<std::path::PathBuf> = ["/usr/share/applications", "/usr/local/share/applications"]
        .iter()
        .map(std::path::PathBuf::from)
        .collect();
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(std::path::Path::new(&home).join(".local/share/applications"));
    }
    let mut out: Vec<AppEntry> = Vec::new();
    for dir in dirs {
        let rd = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for ent in rd.flatten() {
            let path = ent.path();
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            let kf = glib::KeyFile::new();
            if kf.load_from_file(&path, glib::KeyFileFlags::NONE).is_err() {
                continue;
            }
            let grp = "Desktop Entry";
            let s = |k: &str| kf.string(grp, k).map(|g| g.to_string()).unwrap_or_default();
            let b = |k: &str| kf.boolean(grp, k).unwrap_or(false);
            if s("Type") != "Application" || b("NoDisplay") || b("Hidden") {
                continue;
            }
            let name = s("Name");
            let exec = s("Exec");
            if name.is_empty() || exec.is_empty() {
                continue;
            }
            out.push(AppEntry {
                name,
                exec,
                terminal: b("Terminal"),
                icon: s("Icon"),
                bucket: bucket_for(&s("Categories")),
            });
        }
    }
    out.sort_by_key(|a| a.name.to_lowercase());
    out.dedup_by(|a, b| a.name.eq_ignore_ascii_case(&b.name));
    out
}

fn spawn(argv: &[String]) {
    if argv.is_empty() {
        return;
    }
    let _ = Command::new(&argv[0]).args(&argv[1..]).spawn();
}

fn launch_app(app: &AppEntry) {
    let mut argv = exec_argv(&app.exec);
    if app.terminal {
        // GLib can't locate our terminal for Terminal=true entries (htop, vim); wrap in foot.
        let mut w = vec!["foot".to_string(), "-e".to_string()];
        w.append(&mut argv);
        argv = w;
    }
    spawn(&argv);
}

const CSS: &str = "
.menu-root { background: @popover_bg_color; border: 1px solid @borders;
             border-top-right-radius: 12px; }
.rail { background: linear-gradient(to bottom, @accent_bg_color, shade(@accent_bg_color, 0.72));
        min-width: 34px; }
.rail-label { color: alpha(#ffffff, 0.92); font-weight: 800; font-size: 0.82rem; padding: 0; }
.menu-list, .flyout-list { background: transparent; padding: 6px 0; }
.menu-list row, .flyout-list row { padding: 8px 12px; border-radius: 7px; margin: 0 6px; }
.menu-list row:hover, .flyout-list row:hover,
.menu-list row:selected, .flyout-list row:selected {
    background: @accent_bg_color; color: @accent_fg_color; }
.cat-icon, .app-icon { -gtk-icon-size: 20px; }
.chev { color: alpha(@window_fg_color, 0.45); font-size: 0.8em; }
row:hover .chev { color: alpha(@accent_fg_color, 0.85); }
.flyout contents { padding: 4px; min-width: 210px; }
.cat-label, .app-label { margin-left: 2px; }
";

fn load_css() {
    let provider = CssProvider::new();
    provider.load_from_data(CSS);
    if let Some(display) = Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

// A flyout row: an icon + label that runs `action` when clicked (and closes the menu).
fn leaf_row(icon: &str, label: &str, win: &ApplicationWindow, action: Rc<dyn Fn()>) -> ListBoxRow {
    let row = ListBoxRow::new();
    row.set_activatable(true);
    let hb = GtkBox::new(Orientation::Horizontal, 11);
    let img = Image::from_icon_name(&resolve_icon(icon));
    img.add_css_class("app-icon");
    let lbl = Label::new(Some(label));
    lbl.set_halign(Align::Start);
    lbl.set_hexpand(true);
    lbl.add_css_class("app-label");
    hb.append(&img);
    hb.append(&lbl);
    row.set_child(Some(&hb));
    let winw = win.downgrade();
    let click = GestureClick::new();
    click.connect_released(move |_, _, _, _| {
        action();
        if let Some(w) = winw.upgrade() {
            w.close();
        }
    });
    row.add_controller(click);
    row
}

// Add a category row to `list` with a flyout popover of `leaves`.
fn add_category(
    list: &ListBox,
    popovers: &Rc<RefCell<Vec<Popover>>>,
    title: &str,
    icon: &str,
    leaves: Vec<ListBoxRow>,
) {
    if leaves.is_empty() {
        return;
    }
    let row = ListBoxRow::new();
    row.set_activatable(true);
    let hb = GtkBox::new(Orientation::Horizontal, 11);
    let img = Image::from_icon_name(&resolve_icon(icon));
    img.add_css_class("cat-icon");
    let lbl = Label::new(Some(title));
    lbl.set_halign(Align::Start);
    lbl.set_hexpand(true);
    lbl.add_css_class("cat-label");
    let chev = Label::new(Some("\u{25B8}"));
    chev.add_css_class("chev");
    hb.append(&img);
    hb.append(&lbl);
    hb.append(&chev);
    row.set_child(Some(&hb));

    let pop = Popover::new();
    pop.set_parent(&row);
    pop.set_position(PositionType::Right);
    pop.set_autohide(false); // we manage open/close so hover between categories works
    pop.set_has_arrow(false);
    pop.add_css_class("flyout");
    let flist = ListBox::new();
    flist.set_selection_mode(SelectionMode::None);
    flist.add_css_class("flyout-list");
    for leaf in leaves {
        flist.append(&leaf);
    }
    pop.set_child(Some(&flist));
    popovers.borrow_mut().push(pop.clone());

    // Open this flyout (closing others) on hover or click -- Win'95 cascading behaviour. Rc so the
    // one action can be shared by both the motion and click controllers (closures aren't Clone).
    let open: Rc<dyn Fn()> = {
        let pop = pop.clone();
        let all = popovers.clone();
        Rc::new(move || {
            for p in all.borrow().iter() {
                if p != &pop {
                    p.popdown();
                }
            }
            pop.popup();
        })
    };
    let motion = EventControllerMotion::new();
    let o1 = open.clone();
    motion.connect_enter(move |_, _, _| o1());
    row.add_controller(motion);
    let click = GestureClick::new();
    let o2 = open.clone();
    click.connect_released(move |_, _, _, _| o2());
    row.add_controller(click);

    list.append(&row);
}

fn build_menu(app: &Application, apps: &[AppEntry]) -> ApplicationWindow {
    let win = ApplicationWindow::builder().application(app).build();
    win.init_layer_shell();
    win.set_layer(Layer::Overlay);
    // Exclusive keyboard: the surface grabs focus on present, so Escape works AND `is_active`
    // actually toggles -- which is what makes close-on-click-away (below) fire (with OnDemand the
    // surface often never registered active, so the close handler was dead code).
    win.set_keyboard_mode(KeyboardMode::Exclusive);
    win.set_anchor(Edge::Left, true);
    win.set_anchor(Edge::Bottom, true);
    // NO bottom margin: the bottom anchor already sits ABOVE yambar's 26px exclusive zone, so an
    // extra 26px margin double-counted the bar height and left a gap. Anchor alone = flush on the bar.

    let root = GtkBox::new(Orientation::Horizontal, 0);
    root.add_css_class("menu-root");

    // GTK4 removed GtkLabel angle, so the Win'95 vertical banner is a stack of single-char labels,
    // bottom-aligned like the original.
    let rail = GtkBox::new(Orientation::Vertical, 1);
    rail.add_css_class("rail");
    let spacer = GtkBox::new(Orientation::Vertical, 0);
    spacer.set_vexpand(true);
    rail.append(&spacer);
    for ch in "WebGen".chars() {
        let l = Label::new(Some(&ch.to_string()));
        l.add_css_class("rail-label");
        rail.append(&l);
    }
    let pad = GtkBox::new(Orientation::Vertical, 0);
    pad.set_size_request(-1, 12);
    rail.append(&pad);
    root.append(&rail);

    let list = ListBox::new();
    list.set_selection_mode(SelectionMode::None);
    list.add_css_class("menu-list");
    root.append(&list);

    let popovers: Rc<RefCell<Vec<Popover>>> = Rc::new(RefCell::new(Vec::new()));

    // App categories.
    for (bucket, cat_icon) in BUCKETS {
        let leaves: Vec<ListBoxRow> = apps
            .iter()
            .filter(|a| a.bucket == *bucket)
            .map(|a| {
                let ac = a.clone();
                leaf_row(&a.icon, &a.name, &win, Rc::new(move || launch_app(&ac)))
            })
            .collect();
        add_category(&list, &popovers, bucket, cat_icon, leaves);
    }

    // Files -- a top-level shortcut to the file manager, above Run..., so it's one click from the
    // menu root (it ALSO appears under the Utilities bucket -- the duplicate link is by design).
    // NOTE: the launched file manager is hardcoded to webgen-files for now. We WANT this to become
    // a user preference (pick which file manager the menu + "Open folder" actions run) -- not built
    // yet; when it lands, read the chosen command here instead of the literal below.
    {
        let files = leaf_row(
            "system-file-manager",
            "Files",
            &win,
            Rc::new(|| spawn(&["webgen-files".to_string()])),
        );
        list.append(&Separator::new(Orientation::Horizontal));
        list.append(&files);
    }

    // Run... (the old fuzzel search, kept for power users).
    {
        let run = leaf_row(
            "system-search",
            "Run\u{2026}",
            &win,
            Rc::new(|| spawn(&["fuzzel".to_string()])),
        );
        list.append(&Separator::new(Orientation::Horizontal));
        list.append(&run);
    }

    // Power submenu -- mirrors the proven commands from labwc's root menu.
    list.append(&Separator::new(Orientation::Horizontal));
    let power_leaves = vec![
        leaf_row("system-log-out", "Log out", &win,
                 Rc::new(|| spawn(&["pkill".into(), "-TERM".into(), "labwc".into()]))),
        leaf_row("system-reboot", "Reboot", &win,
                 Rc::new(|| spawn(&["sh".into(), "-c".into(), "sudo /etc/rc.shutdown reboot".into()]))),
        leaf_row("system-shutdown", "Power off", &win,
                 Rc::new(|| spawn(&["sh".into(), "-c".into(), "sudo /etc/rc.shutdown poweroff".into()]))),
    ];
    add_category(&list, &popovers, "Power", "system-shutdown", power_leaves);

    win.set_child(Some(&root));

    // Dismiss: Escape, or focus loss (click anywhere outside -- including the panel button).
    let keys = EventControllerKey::new();
    let winw = win.downgrade();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gtk::gdk::Key::Escape {
            if let Some(w) = winw.upgrade() {
                w.close();
            }
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    win.add_controller(keys);

    let activated_once = Rc::new(RefCell::new(false));
    let winw = win.downgrade();
    let pops = popovers.clone();
    win.connect_is_active_notify(move |w| {
        if w.is_active() {
            *activated_once.borrow_mut() = true;
        } else if *activated_once.borrow() {
            // Clicked away after the menu had focus: close it and any open flyout. With the
            // Exclusive keyboard mode set above, is_active is now reliable so this actually fires.
            for p in pops.borrow().iter() {
                p.popdown();
            }
            if let Some(w) = winw.upgrade() {
                w.close();
            }
        }
    });

    win.present();
    win
}

fn main() -> glib::ExitCode {
    // Single-instance (NOT non-unique): every `webgen-menu` invocation -- the taskbar button and
    // the desktop left/right click all run it -- lands on the one primary instance and TOGGLES the
    // menu (open if closed, close if open). No hold() is needed: gio keeps the primary alive across
    // a remote activation, so the toggle handle + debounce below are valid exactly when a re-invoke
    // arrives; the primary then quits when the menu closes (non-resident -- leaner).
    let app = Application::builder().application_id(APP_ID).build();
    app.connect_startup(|_| load_css());

    let current: Rc<RefCell<Option<ApplicationWindow>>> = Rc::new(RefCell::new(None));
    let last_close = Rc::new(RefCell::new(None::<Instant>));
    {
        let current = current.clone();
        let last_close = last_close.clone();
        app.connect_activate(move |app| {
            // Toggle OFF: a menu is already open -> close it. (Bind first so the RefCell borrow ends
            // before close() may synchronously fire the close handler below.)
            let open = current.borrow_mut().take();
            if let Some(w) = open {
                w.close();
                *last_close.borrow_mut() = Some(Instant::now());
                return;
            }
            // Debounce: the SAME click that defocuses+closes the menu (taskbar button / desktop click)
            // also re-invokes us; without this we'd immediately reopen. 300ms absorbs that race.
            if let Some(t) = *last_close.borrow() {
                if t.elapsed() < Duration::from_millis(300) {
                    return;
                }
            }
            let apps = load_apps();
            let win = build_menu(app, &apps);
            {
                // Focus-loss / Escape / launching an app closes the window -> clear our handle and
                // stamp the time so the next invocation toggles cleanly.
                let current = current.clone();
                let last_close = last_close.clone();
                win.connect_close_request(move |_| {
                    *current.borrow_mut() = None;
                    *last_close.borrow_mut() = Some(Instant::now());
                    glib::Propagation::Proceed
                });
            }
            *current.borrow_mut() = Some(win);
        });
    }
    app.run()
}
