//! The Hopwyn window (GTK 4 + libadwaita).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use hopwyn_proto::Edge;
use tokio::sync::mpsc::UnboundedSender;

use crate::config::Partner;
use crate::config::{AskWhen, MAX_PARTNERS};
use crate::layout::{self, Size};
use crate::service::{self, Command, Control, InputState, Nearby, UiEvent};
use hopwyn_pair::DeviceId;

pub const APP_ID: &str = "app.hopwyn.Hopwyn";

const SIDES: [&str; 4] = [
    "Left of this screen",
    "Right of this screen",
    "Above this screen",
    "Below this screen",
];

const CSS: &str = "
.status-card { padding: 28px 24px; }
.status-card image { color: @accent_color; }
.status-card.inactive image { color: @warning_color; }
.status-card.problem image { color: @error_color; }
.pairing-code { font-size: 44px; font-weight: 800; letter-spacing: 6px; font-feature-settings: 'tnum'; }
";

fn edge_index(edge: Edge) -> u32 {
    match edge {
        Edge::Left => 0,
        Edge::Right => 1,
        Edge::Top => 2,
        Edge::Bottom => 3,
    }
}

fn index_edge(i: u32) -> Option<Edge> {
    match i {
        0 => Some(Edge::Left),
        1 => Some(Edge::Right),
        2 => Some(Edge::Top),
        3 => Some(Edge::Bottom),
        _ => None,
    }
}

fn edge_words(edge: Edge) -> &'static str {
    match edge {
        Edge::Left => "left",
        Edge::Right => "right",
        Edge::Top => "top",
        Edge::Bottom => "bottom",
    }
}

/// `--background` (used at login) starts with the window hidden.
pub fn run() -> glib::ExitCode {
    let background = std::env::args().any(|a| a == "--background");
    let mut builder = adw::Application::builder().application_id(APP_ID);
    // Test copies (see service::no_input) must not merge into one app.
    if std::env::var_os("HOPWYN_NO_INPUT").is_some() {
        builder = builder.flags(gtk::gio::ApplicationFlags::NON_UNIQUE);
    }
    let app = builder.build();
    let first = Cell::new(true);
    app.connect_activate(move |app| {
        if let Some(win) = app.windows().first() {
            win.present();
            return;
        }
        // Started at login: stay hidden. If a permission dialog is needed
        // later, the window comes up for it and hides again afterwards.
        let window = build(app);
        if !(background && first.get()) {
            window.present();
        }
        first.set(false);
    });
    let quit = gtk::gio::SimpleAction::new("quit", None);
    {
        let app2 = app.clone();
        quit.connect_activate(move |_, _| app2.quit());
    }
    app.add_action(&quit);
    app.set_accels_for_action("app.quit", &["<Control>q"]);
    // Keep running with no window open, so the link and the desktop's
    // permissions stay alive and the dialogs are not asked again. Only the
    // primary copy holds: startup doesn't run in a copy that just hands
    // its launch to the running one, which then exits.
    app.connect_startup(|app| std::mem::forget(app.hold()));
    // GTK must not see our own flags.
    let argv0: Vec<String> = std::env::args().take(1).collect();
    app.run_with_args(&argv0)
}

struct Ui {
    window: adw::ApplicationWindow,
    window_title: adw::WindowTitle,
    toasts: adw::ToastOverlay,
    banner: adw::Banner,
    card: gtk::Box,
    status_icon: gtk::Image,
    status_title: gtk::Label,
    status_text: gtk::Label,
    partner_group: adw::PreferencesGroup,
    /// The layout editor: this desktop's monitors and the online computers
    /// around it, to scale; drag a computer to move it.
    canvas: gtk::DrawingArea,
    /// This desktop's size and its monitors (x, y, width, height relative to
    /// the desktop's top-left), in logical pixels.
    my_size: Cell<Option<Size>>,
    monitors: RefCell<Vec<(f64, f64, f64, f64)>>,
    peer_sizes: RefCell<HashMap<DeviceId, Size>>,
    /// Paired phones used as remotes: no side to choose, not in the picture.
    remotes: RefCell<std::collections::HashSet<DeviceId>>,
    drag: RefCell<Option<Drag>>,
    /// Canvas transform from the last draw: (scale, x origin, y origin).
    view: Cell<(f64, f64, f64)>,
    partner_rows: RefCell<Vec<(DeviceId, adw::ActionRow)>>,
    empty_row: adw::ActionRow,
    nearby_group: adw::PreferencesGroup,
    nearby_empty: adw::ActionRow,
    nearby_rows: RefCell<Vec<adw::ActionRow>>,
    code_dialog: RefCell<Option<adw::AlertDialog>>,
    /// "Waiting for … to click Pair", dismissed when pairing ends.
    waiting_toast: RefCell<Option<adw::Toast>>,
    cmd: UnboundedSender<Command>,
    partners: RefCell<Vec<Partner>>,
    status: RefCell<(Vec<DeviceId>, Control)>,
    ready: Cell<bool>,
    failed: Cell<bool>,
    /// The window was hidden when a permission dialog needed it: hide it
    /// again once permission is granted.
    hide_after_input: Cell<bool>,
    /// The window should hand itself to the service once it's on screen.
    export_pending: Cell<bool>,
    sharing_row: adw::SwitchRow,
    ask_row: adw::ComboRow,
    /// Set while the rows are updated from the service, so that doesn't
    /// echo back as a command.
    updating: Cell<bool>,
}

fn build(app: &adw::Application) -> adw::ApplicationWindow {
    if let Some(display) = gtk::gdk::Display::default() {
        let css = gtk::CssProvider::new();
        css.load_from_data(CSS);
        gtk::style_context_add_provider_for_display(
            &display,
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        // Lets a development build find its icon before it is installed.
        gtk::IconTheme::for_display(&display)
            .add_search_path(concat!(env!("CARGO_MANIFEST_DIR"), "/data/icons"));
    }
    gtk::Window::set_default_icon_name(APP_ID);

    let (ui_tx, ui_rx) = async_channel::unbounded();
    let cmd = service::spawn(ui_tx);

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Hopwyn")
        .default_width(480)
        .default_height(720)
        .build();

    let header = adw::HeaderBar::new();
    let window_title = adw::WindowTitle::new("Hopwyn", "");
    header.set_title_widget(Some(&window_title));
    let menu = gtk::gio::Menu::new();
    menu.append(Some("Quit Hopwyn"), Some("app.quit"));
    let menu_button = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&menu)
        .tooltip_text("Menu")
        .build();
    header.pack_end(&menu_button);
    let banner = adw::Banner::new("");
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.add_top_bar(&banner);

    // Status card.
    let status_icon = gtk::Image::builder()
        .icon_name("network-transmit-receive-symbolic")
        .pixel_size(64)
        .build();
    let status_title = gtk::Label::builder()
        .label("Starting…")
        .css_classes(["title-2"])
        .wrap(true)
        .justify(gtk::Justification::Center)
        .build();
    let status_text = gtk::Label::builder()
        .label("Setting up input sharing")
        .css_classes(["dim-label"])
        .wrap(true)
        .justify(gtk::Justification::Center)
        .build();
    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .css_classes(["card", "status-card"])
        .build();
    card.append(&status_icon);
    card.append(&status_title);
    card.append(&status_text);

    // Sharing on/off, and when to ask for the desktop's permission.
    let sharing_group = adw::PreferencesGroup::builder().title("Sharing").build();
    let sharing_row = adw::SwitchRow::builder()
        .title("Share mouse and keyboard")
        .active(true)
        .build();
    let ask_row = adw::ComboRow::builder()
        .title("Ask for permission")
        .subtitle("When needed: only once a paired computer is online. GNOME forgets the answer at logout.")
        .model(&gtk::StringList::new(&["When needed", "At startup"]))
        .build();
    sharing_group.add(&sharing_row);
    sharing_group.add(&ask_row);

    // Paired computers, with a picture of where each one sits.
    let partner_group = adw::PreferencesGroup::builder()
        .title("Paired computers")
        .description(format!(
            "Up to {MAX_PARTNERS}. Drag a computer in the picture to where its screen really is, along any edge; the other computer updates too. Computers that are never on together (home and office) can share a side."
        ))
        .build();
    let canvas = gtk::DrawingArea::builder()
        .content_height(240)
        .hexpand(true)
        .margin_bottom(12)
        .visible(false)
        .build();
    partner_group.add(&canvas);
    let empty_row = adw::ActionRow::builder()
        .title("No computer paired yet")
        .subtitle("Click Add computer here, or Pair on a computer listed under Nearby")
        .build();
    partner_group.add(&empty_row);

    // Add a device.
    let add_group = adw::PreferencesGroup::builder()
        .title("Add a computer")
        .description(
            "Open Hopwyn on the other computer too. Both show the same code; click Pair on each.",
        )
        .build();
    let add_row = adw::ActionRow::builder()
        .title("Pair a computer")
        .subtitle("Nearby computers running Hopwyn are asked automatically")
        .activatable(true)
        .build();
    add_row.add_prefix(&gtk::Image::from_icon_name("list-add-symbolic"));
    let add_button = gtk::Button::builder()
        .label("Add computer")
        .valign(gtk::Align::Center)
        .css_classes(["suggested-action"])
        .build();
    add_row.add_suffix(&add_button);
    add_row.set_activatable_widget(Some(&add_button));
    add_group.add(&add_row);

    // Nearby.
    let nearby_group = adw::PreferencesGroup::builder()
        .title("Nearby")
        .description("Computers running Hopwyn on this network")
        .build();
    let nearby_empty = adw::ActionRow::builder()
        .title("Looking for computers…")
        .build();
    let spinner = gtk::Spinner::builder()
        .spinning(true)
        .valign(gtk::Align::Center)
        .build();
    nearby_empty.add_suffix(&spinner);
    nearby_group.add(&nearby_empty);

    // Tips.
    let tips = adw::PreferencesGroup::builder()
        .title("Good to know")
        .build();
    let tip_panic = adw::ActionRow::builder()
        .title("Ctrl + Alt + Esc")
        .subtitle("Gets your cursor back at any time")
        .build();
    tip_panic.add_prefix(&gtk::Image::from_icon_name("input-keyboard-symbolic"));
    let tip_private = adw::ActionRow::builder()
        .title("Private by design")
        .subtitle("Encrypted, home network only, no account or cloud")
        .build();
    tip_private.add_prefix(&gtk::Image::from_icon_name("security-high-symbolic"));
    let tip_background = adw::ActionRow::builder()
        .title("Keeps working when closed")
        .subtitle("Closing this window leaves Hopwyn running. Quit from the menu or with Ctrl + Q")
        .build();
    tip_background.add_prefix(&gtk::Image::from_icon_name("window-close-symbolic"));
    tips.add(&tip_panic);
    tips.add(&tip_background);
    tips.add(&tip_private);

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(24)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(16)
        .margin_end(16)
        .build();
    content.append(&card);
    content.append(&sharing_group);
    content.append(&partner_group);
    content.append(&add_group);
    content.append(&nearby_group);
    content.append(&tips);
    let clamp = adw::Clamp::builder()
        .maximum_size(560)
        .child(&content)
        .build();
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&clamp)
        .build();
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&scroller));
    toolbar.set_content(Some(&toasts));
    window.set_content(Some(&toolbar));

    let ui = Rc::new(Ui {
        window: window.clone(),
        window_title,
        toasts,
        banner,
        card,
        status_icon,
        status_title,
        status_text,
        partner_group,
        canvas: canvas.clone(),
        my_size: Cell::new(None),
        monitors: RefCell::new(Vec::new()),
        peer_sizes: RefCell::new(HashMap::new()),
        remotes: RefCell::new(std::collections::HashSet::new()),
        drag: RefCell::new(None),
        view: Cell::new((1.0, 0.0, 0.0)),
        partner_rows: RefCell::new(Vec::new()),
        empty_row,
        nearby_group,
        nearby_empty,
        nearby_rows: RefCell::new(Vec::new()),
        code_dialog: RefCell::new(None),
        waiting_toast: RefCell::new(None),
        cmd,
        partners: RefCell::new(Vec::new()),
        status: RefCell::new((Vec::new(), Control::Local)),
        ready: Cell::new(false),
        failed: Cell::new(false),
        hide_after_input: Cell::new(false),
        export_pending: Cell::new(false),
        sharing_row: sharing_row.clone(),
        ask_row: ask_row.clone(),
        updating: Cell::new(false),
    });

    {
        let ui2 = ui.clone();
        add_button.connect_clicked(move |_| ui2.show_code_dialog());
    }
    {
        let ui2 = ui.clone();
        // Closing hides the window; Hopwyn keeps running.
        window.connect_close_request(move |w| {
            w.set_visible(false);
            glib::Propagation::Stop
        });
        app.connect_shutdown(move |_| {
            let _ = ui2.cmd.send(Command::Quit);
        });
    }
    {
        let ui2 = ui.clone();
        glib::spawn_future_local(async move {
            while let Ok(ev) = ui_rx.recv().await {
                ui2.handle(ev);
            }
        });
    }

    // The layout editor: draw it, and drag computers around.
    {
        let ui2 = ui.clone();
        canvas.set_draw_func(move |_, cr, w, h| ui2.draw_layout(cr, f64::from(w), f64::from(h)));
    }
    {
        let drag = gtk::GestureDrag::new();
        let ui2 = ui.clone();
        drag.connect_drag_begin(move |_, x, y| ui2.drag_begin(x, y));
        let ui2 = ui.clone();
        drag.connect_drag_update(move |_, dx, dy| ui2.drag_update(dx, dy));
        let ui2 = ui.clone();
        drag.connect_drag_end(move |_, dx, dy| ui2.drag_end(dx, dy));
        canvas.add_controller(drag);
    }
    // This desktop's monitors, now and whenever they change.
    if let Some(display) = gtk::gdk::Display::default() {
        ui.read_monitors(&display);
        let ui2 = ui.clone();
        let d2 = display.clone();
        display
            .monitors()
            .connect_items_changed(move |_, _, _, _| ui2.read_monitors(&d2));
    }

    {
        let ui2 = ui.clone();
        ui.sharing_row.connect_active_notify(move |row| {
            if !ui2.updating.get() {
                let _ = ui2.cmd.send(Command::SetSharing(row.is_active()));
            }
        });
    }
    {
        let ui2 = ui.clone();
        ui.ask_row.connect_selected_notify(move |row| {
            if !ui2.updating.get() {
                let ask = if row.selected() == 1 {
                    AskWhen::Startup
                } else {
                    AskWhen::Online
                };
                let _ = ui2.cmd.send(Command::SetAskWhen(ask));
            }
        });
    }
    {
        let ui2 = ui.clone();
        ui.banner.connect_button_clicked(move |_| {
            let _ = ui2.cmd.send(Command::AskAgain);
        });
    }
    // When a permission dialog is wanted and the window had to be shown
    // first, hand it to the service once it's on screen.
    {
        let ui2 = ui.clone();
        window.connect_map(move |w| {
            if ui2.export_pending.replace(false) {
                export_window(w, ui2.cmd.clone());
            }
        });
    }

    window
}

/// Tells the service to ask for permission, with `window` (as
/// `wayland:<handle>` or `x11:<xid>`) for the dialog to open on.
fn export_window(window: &adw::ApplicationWindow, cmd: UnboundedSender<Command>) {
    let start = move |parent: Option<String>| {
        let _ = cmd.send(Command::StartInput { parent });
    };
    let Some(surface) = window.surface() else {
        start(None);
        return;
    };
    if let Some(top) = surface.downcast_ref::<gdk_wayland::WaylandToplevel>() {
        let start2 = start.clone();
        let exported = top.export_handle(move |_, handle| {
            start2(handle.ok().map(|h| format!("wayland:{h}")));
        });
        if !exported {
            start(None);
        }
        return;
    }
    if let Some(x) = surface.downcast_ref::<gdk_x11::X11Surface>() {
        start(Some(format!("x11:{:x}", x.xid())));
        return;
    }
    start(None);
}

impl Ui {
    fn handle(self: &Rc<Self>, ev: UiEvent) {
        if matches!(ev, UiEvent::Fatal(_)) {
            self.failed.set(true);
        }
        match ev {
            UiEvent::Waiting(text) => {
                self.banner.set_title(&format!(
                    "{text}. If you don't see it, press the Super key."
                ));
                self.banner.set_revealed(true);
                self.set_card(
                    "preferences-system-privacy-symbolic",
                    "Waiting for permission",
                    &text,
                    "",
                );
            }
            UiEvent::Ready { name, short_id } => {
                self.window_title
                    .set_subtitle(&format!("{name} · {short_id}"));
                self.banner.set_revealed(false);
                self.ready.set(true);
                self.refresh_card();
            }
            UiEvent::Settings(settings) => {
                self.updating.set(true);
                self.sharing_row.set_active(settings.sharing);
                self.ask_row.set_selected(match settings.ask {
                    AskWhen::Online => 0,
                    AskWhen::Startup => 1,
                });
                self.updating.set(false);
            }
            UiEvent::Input(state) => self.show_input(state),
            UiEvent::AskPermission => {
                if !self.window.is_visible() {
                    self.hide_after_input.set(true);
                }
                if self.window.is_mapped() {
                    self.window.present();
                    export_window(&self.window, self.cmd.clone());
                } else {
                    self.export_pending.set(true);
                    self.window.present();
                }
            }
            UiEvent::Notice(text) => self.toast(&text),
            UiEvent::Remote(id) => {
                self.remotes.borrow_mut().insert(id);
                self.refresh_partners();
            }
            UiEvent::PeerScreen { id, size } => {
                self.peer_sizes.borrow_mut().insert(id, size);
                self.canvas.queue_draw();
            }

            UiEvent::Status { connected, control } => {
                *self.status.borrow_mut() = (connected, control);
                self.refresh_card();
                self.refresh_partner_status();
            }
            UiEvent::Partners(list) => {
                *self.partners.borrow_mut() = list;
                self.refresh_partners();
                self.refresh_card();
            }
            UiEvent::Nearby(list) => self.refresh_nearby(&list),
            UiEvent::PairingOpen => self.show_waiting(),
            UiEvent::PairingConfirm { name, code } => self.show_compare(&name, &code),
            UiEvent::PairingClosed => {
                self.close_code_dialog();
                self.dismiss_waiting();
            }
            UiEvent::PairingDone(name) => {
                self.close_code_dialog();
                self.dismiss_waiting();
                self.toast(&format!("Paired with {name}"));
            }
            UiEvent::PairingFailed(why) => {
                self.close_code_dialog();
                self.dismiss_waiting();
                self.toast(&format!("Pairing failed: {why}"));
            }
            UiEvent::Error(e) | UiEvent::Fatal(e) => {
                self.banner.set_revealed(false);
                self.set_card(
                    "dialog-error-symbolic",
                    "Something went wrong",
                    &e,
                    "problem",
                );
            }
        }
    }

    /// Shows where the input permission stands.
    fn show_input(&self, state: InputState) {
        let (subtitle, banner) = match state {
            InputState::Off => (
                "Off: other computers can't use or be used from this one",
                None,
            ),
            InputState::Waiting => ("Will ask for permission when it's needed", None),
            InputState::Asking => (
                "Asking for permission",
                Some(
                    "Allow Hopwyn in the desktop's dialog. If you don't see it, press the Super key.",
                ),
            ),
            InputState::Ready => ("On: the screen edges are live", None),
            InputState::Refused => (
                "Permission not given",
                Some("Hopwyn needs permission to use the screen edges."),
            ),
            InputState::Disabled => ("Test mode: no input", None),
        };
        self.sharing_row.set_subtitle(subtitle);
        match banner {
            Some(text) => {
                self.banner.set_title(text);
                self.banner
                    .set_button_label((state == InputState::Refused).then_some("Allow…"));
                self.banner.set_revealed(true);
            }
            None => self.banner.set_revealed(false),
        }
        if state == InputState::Ready && self.hide_after_input.replace(false) {
            self.window.set_visible(false);
        }
        if state != InputState::Asking {
            self.refresh_card();
        }
    }

    fn dismiss_waiting(&self) {
        let toast = self.waiting_toast.borrow_mut().take();
        if let Some(t) = toast {
            t.dismiss();
        }
    }

    fn toast(&self, text: &str) {
        self.toasts.add_toast(adw::Toast::new(text));
    }

    fn set_card(&self, icon: &str, title: &str, text: &str, class: &str) {
        self.status_icon.set_icon_name(Some(icon));
        self.status_title.set_label(title);
        self.status_text.set_label(text);
        for c in ["inactive", "problem"] {
            self.card.remove_css_class(c);
        }
        if !class.is_empty() {
            self.card.add_css_class(class);
        }
    }

    fn partner_name(&self, id: &DeviceId) -> String {
        self.partners
            .borrow()
            .iter()
            .find(|p| &p.id == id)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "the other computer".into())
    }

    fn refresh_card(&self) {
        if !self.ready.get() {
            return;
        }
        let partners = self.partners.borrow().clone();
        let (connected, control) = self.status.borrow().clone();
        if partners.is_empty() {
            self.set_card(
                "computer-symbolic",
                "No computer paired",
                "Pair a computer below to share this mouse and keyboard with it.",
                "",
            );
            return;
        }
        match control {
            Control::Controlling(id) => self.set_card(
                "go-next-symbolic",
                &format!("Controlling {}", self.partner_name(&id)),
                "This computer is inactive while its mouse and keyboard drive the other one.",
                "inactive",
            ),
            Control::ControlledBy(id) => {
                let name = self.partner_name(&id);
                self.set_card(
                    "go-previous-symbolic",
                    &format!("Controlled by {name}"),
                    &format!("{name}'s mouse and keyboard are driving this computer."),
                    "",
                );
            }
            Control::Local => {
                let online: Vec<String> = partners
                    .iter()
                    .filter(|p| connected.contains(&p.id))
                    .map(|p| format!("the {} edge for {}", edge_words(p.edge), p.name))
                    .collect();
                if online.is_empty() {
                    let names: Vec<&str> = partners.iter().map(|p| p.name.as_str()).collect();
                    self.set_card(
                        "network-wireless-symbolic",
                        &format!("Looking for {}", join_words(&names)),
                        "Make sure Hopwyn is open on them and all are on the same network.",
                        "",
                    );
                } else {
                    let refs: Vec<&str> = online.iter().map(String::as_str).collect();
                    self.set_card(
                        "input-mouse-symbolic",
                        "Ready",
                        &format!("Move the cursor off {}.", join_words(&refs)),
                        "",
                    );
                }
            }
        }
    }

    /// Updates each paired computer's subtitle and the layout picture.
    fn refresh_partner_status(&self) {
        let (connected, control) = self.status.borrow().clone();
        let partners = self.partners.borrow();
        for (id, row) in self.partner_rows.borrow().iter() {
            let active =
                matches!(control, Control::Controlling(c) | Control::ControlledBy(c) if c == *id);
            let state = if active {
                "In use"
            } else if connected.contains(id) {
                "Connected"
            } else {
                "Not connected"
            };
            row.set_subtitle(state);
        }
        self.canvas.set_visible(!partners.is_empty());
        self.canvas.queue_draw();
    }

    /// Rebuilds one row per paired computer.
    fn refresh_partners(self: &Rc<Self>) {
        for (_, row) in self.partner_rows.borrow_mut().drain(..) {
            self.partner_group.remove(&row);
        }
        if self.empty_row.parent().is_some() {
            self.partner_group.remove(&self.empty_row);
        }
        let partners = self.partners.borrow().clone();
        if partners.is_empty() {
            self.partner_group.add(&self.empty_row);
        }
        for p in partners {
            // A plain row with its own controls: an activatable row would
            // take the clicks meant for the Forget button.
            let row = adw::ActionRow::builder().title(&p.name).build();
            row.add_prefix(&gtk::Image::from_icon_name("computer-symbolic"));
            let side = gtk::DropDown::builder()
                .model(&gtk::StringList::new(&SIDES))
                .valign(gtk::Align::Center)
                .tooltip_text(format!("Where {}'s screen is", p.name))
                .build();
            side.set_selected(edge_index(p.edge));
            if self.remotes.borrow().contains(&p.id) {
                row.set_title(&format!("{} (phone remote)", p.name));
            } else {
                row.add_suffix(&side);
            }
            let forget = gtk::Button::builder()
                .icon_name("user-trash-symbolic")
                .tooltip_text(format!("Forget {}", p.name))
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .build();
            row.add_suffix(&forget);
            {
                let ui = self.clone();
                let (id, name) = (p.id, p.name.clone());
                forget.connect_clicked(move |_| ui.confirm_forget(id, &name));
            }
            {
                // Connected after set_selected, so building the row sends nothing.
                let ui = self.clone();
                let id = p.id;
                side.connect_selected_notify(move |side| {
                    if let Some(edge) = index_edge(side.selected()) {
                        let _ = ui.cmd.send(Command::SetEdge { id, edge });
                    }
                });
            }
            self.partner_group.add(&row);
            self.partner_rows.borrow_mut().push((p.id, row));
        }
        self.refresh_partner_status();
    }

    fn refresh_nearby(self: &Rc<Self>, list: &[Nearby]) {
        for row in self.nearby_rows.borrow_mut().drain(..) {
            self.nearby_group.remove(&row);
        }
        if list.is_empty() {
            if self.nearby_empty.parent().is_none() {
                self.nearby_group.add(&self.nearby_empty);
            }
            return;
        }
        if self.nearby_empty.parent().is_some() {
            self.nearby_group.remove(&self.nearby_empty);
        }
        for n in list {
            let subtitle = if n.paired || n.open {
                n.ip.to_string()
            } else {
                format!("{} · Click Add computer on it to pair", n.ip)
            };
            let row = adw::ActionRow::builder()
                .title(&n.name)
                .subtitle(subtitle)
                .build();
            row.add_prefix(&gtk::Image::from_icon_name("computer-symbolic"));
            if n.paired {
                let tag = gtk::Label::builder()
                    .label("Paired")
                    .css_classes(["dim-label"])
                    .build();
                row.add_suffix(&tag);
            } else if n.open {
                let button = gtk::Button::builder()
                    .label("Pair")
                    .valign(gtk::Align::Center)
                    .css_classes(["suggested-action"])
                    .build();
                let ui = self.clone();
                let id = n.id;
                button.connect_clicked(move |_| ui.join(id));
                row.add_suffix(&button);
                row.set_activatable_widget(Some(&button));
            }
            self.nearby_group.add(&row);
            self.nearby_rows.borrow_mut().push(row);
        }
    }

    fn show_code_dialog(self: &Rc<Self>) {
        if !self.check_ready() {
            return;
        }
        if self.partners.borrow().len() >= MAX_PARTNERS {
            self.toast("Hopwyn is paired with 4 computers, one on each side. Forget one first");
            return;
        }
        let _ = self.cmd.send(Command::AddDevice);
    }

    /// Shows `dialog` as the current pairing dialog. If the user closes
    /// it (rather than the app replacing it), `on_user` gets the response.
    fn set_code_dialog(
        self: &Rc<Self>,
        dialog: &adw::AlertDialog,
        on_user: impl Fn(&Rc<Self>, &str) + 'static,
    ) {
        self.close_code_dialog();
        let ui = self.clone();
        dialog.connect_response(None, move |_, response| {
            // Still stored means the user answered; close_code_dialog takes
            // it out first, so closing it from code doesn't count.
            let by_user = ui.code_dialog.borrow_mut().take().is_some();
            if by_user {
                on_user(&ui, response);
            }
        });
        dialog.present(Some(&self.window));
        *self.code_dialog.borrow_mut() = Some(dialog.clone());
    }

    /// "Add computer" is open: tell the user what happens next.
    fn show_waiting(self: &Rc<Self>) {
        let spinner = gtk::Spinner::builder()
            .spinning(true)
            .width_request(32)
            .height_request(32)
            .halign(gtk::Align::Center)
            .build();
        let dialog = adw::AlertDialog::new(
            Some("Waiting for the other computer"),
            Some(
                "Open Hopwyn on the other computer. It will show a pairing code on its own within a few seconds.",
            ),
        );
        dialog.set_extra_child(Some(&spinner));
        dialog.add_response("cancel", "Cancel");
        dialog.set_close_response("cancel");
        self.set_code_dialog(&dialog, |ui, _| {
            let _ = ui.cmd.send(Command::CancelPairing);
        });
    }

    /// Both computers show this code; pair only if they match.
    fn show_compare(self: &Rc<Self>, name: &str, code: &str) {
        // Hopwyn may be running with its window closed.
        self.window.present();
        let spaced = format!(
            "{} {}",
            code.get(..3).unwrap_or(""),
            code.get(3..).unwrap_or("")
        );
        let label = gtk::Label::builder()
            .label(&spaced)
            .css_classes(["pairing-code"])
            .selectable(true)
            .build();
        let dialog = adw::AlertDialog::new(
            Some(&format!("Pair with {name}?")),
            Some(&format!(
                "Check that {name} shows the same code, then click Pair on both computers."
            )),
        );
        dialog.set_extra_child(Some(&label));
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("pair", "Pair");
        dialog.set_response_appearance("pair", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("pair"));
        dialog.set_close_response("cancel");
        let name = name.to_owned();
        self.set_code_dialog(&dialog, move |ui, response| {
            let yes = response == "pair";
            let _ = ui.cmd.send(Command::ConfirmPairing(yes));
            if yes {
                let toast = adw::Toast::new(&format!("Waiting for {name} to click Pair…"));
                toast.set_timeout(0);
                ui.toasts.add_toast(toast.clone());
                *ui.waiting_toast.borrow_mut() = Some(toast);
            }
        });
    }

    fn close_code_dialog(&self) {
        // Release the borrow before closing: force_close runs the response
        // handler, which borrows code_dialog too.
        let dialog = self.code_dialog.borrow_mut().take();
        if let Some(d) = dialog {
            d.force_close();
        }
    }

    /// Whether the service can act on commands; tells the user why not.
    fn check_ready(&self) -> bool {
        if self.failed.get() {
            self.toast("Hopwyn stopped because of the error above. Quit and open it again");
            return false;
        }
        if !self.ready.get() {
            self.toast("Allow the desktop's permission dialog first (press Super to find it)");
            return false;
        }
        true
    }

    fn join(self: &Rc<Self>, id: DeviceId) {
        if self.check_ready() {
            let _ = self.cmd.send(Command::JoinDevice(id));
        }
    }

    fn confirm_forget(self: &Rc<Self>, id: DeviceId, name: &str) {
        let dialog = adw::AlertDialog::new(
            Some(&format!("Forget {name}?")),
            Some("It will no longer be able to connect. You can pair again later."),
        );
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("forget", "Forget");
        dialog.set_response_appearance("forget", adw::ResponseAppearance::Destructive);
        dialog.set_close_response("cancel");
        let ui = self.clone();
        dialog.connect_response(None, move |_, response| {
            if response == "forget" {
                let _ = ui.cmd.send(Command::Forget(id));
            }
        });
        dialog.present(Some(&self.window));
    }
}

/// "a", "a and b", "a, b and c".
fn join_words(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// A computer being dragged in the layout editor (world coordinates are
/// this desktop's logical pixels, its top-left at 0,0).
#[derive(Debug, Clone, Copy)]
struct Drag {
    id: DeviceId,
    /// Its rectangle when the drag began.
    x0: f64,
    y0: f64,
    /// Where it is now.
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

const DEFAULT_SIZE: Size = Size {
    width: 1920,
    height: 1080,
};

impl Ui {
    /// Reads this desktop's monitors and tells the service its size.
    fn read_monitors(&self, display: &gtk::gdk::Display) {
        let list = display.monitors();
        let mut rects = Vec::new();
        for i in 0..list.n_items() {
            if let Some(m) = list
                .item(i)
                .and_then(|o| o.downcast::<gtk::gdk::Monitor>().ok())
            {
                let g = m.geometry();
                rects.push((
                    f64::from(g.x()),
                    f64::from(g.y()),
                    f64::from(g.width()),
                    f64::from(g.height()),
                ));
            }
        }
        if rects.is_empty() {
            return;
        }
        let min_x = rects.iter().map(|r| r.0).fold(f64::MAX, f64::min);
        let min_y = rects.iter().map(|r| r.1).fold(f64::MAX, f64::min);
        let max_x = rects.iter().map(|r| r.0 + r.2).fold(f64::MIN, f64::max);
        let max_y = rects.iter().map(|r| r.1 + r.3).fold(f64::MIN, f64::max);
        for r in &mut rects {
            r.0 -= min_x;
            r.1 -= min_y;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let size = Size {
            width: (max_x - min_x).max(1.0) as u32,
            height: (max_y - min_y).max(1.0) as u32,
        };
        *self.monitors.borrow_mut() = rects;
        self.my_size.set(Some(size));
        let _ = self.cmd.send(Command::SetScreen(size));
        self.canvas.queue_draw();
    }

    /// The online computers' rectangles, in world coordinates.
    fn peer_rects(&self) -> Vec<(Partner, (f64, f64, f64, f64))> {
        let mine = self.my_size.get().unwrap_or(DEFAULT_SIZE);
        let (mw, mh) = (f64::from(mine.width), f64::from(mine.height));
        let connected = self.status.borrow().0.clone();
        let sizes = self.peer_sizes.borrow();
        let drag = *self.drag.borrow();
        self.partners
            .borrow()
            .iter()
            .filter(|p| connected.contains(&p.id) && !self.remotes.borrow().contains(&p.id))
            .map(|p| {
                let size = sizes.get(&p.id).copied().unwrap_or(mine);
                let (w, h) = (f64::from(size.width), f64::from(size.height));
                let o = f64::from(p.offset);
                let rect = match drag {
                    Some(d) if d.id == p.id => (d.x, d.y, d.w, d.h),
                    _ => match p.edge {
                        Edge::Right => (mw, o, w, h),
                        Edge::Left => (-w, o, w, h),
                        Edge::Top => (o, -h, w, h),
                        Edge::Bottom => (o, mh, w, h),
                    },
                };
                (p.clone(), rect)
            })
            .collect()
    }

    fn draw_layout(&self, cr: &gtk::cairo::Context, width: f64, height: f64) {
        let mine = self.my_size.get().unwrap_or(DEFAULT_SIZE);
        let (mw, mh) = (f64::from(mine.width), f64::from(mine.height));
        let peers = self.peer_rects();
        // Fit everything, with a margin.
        let (mut x0, mut y0, mut x1, mut y1) = (0.0_f64, 0.0_f64, mw, mh);
        for (_, (x, y, w, h)) in &peers {
            x0 = x0.min(*x);
            y0 = y0.min(*y);
            x1 = x1.max(x + w);
            y1 = y1.max(y + h);
        }
        let margin = 12.0;
        let scale = ((width - 2.0 * margin) / (x1 - x0))
            .min((height - 2.0 * margin) / (y1 - y0))
            .max(0.0001);
        let ox = (width - (x1 - x0) * scale) / 2.0 - x0 * scale;
        let oy = (height - (y1 - y0) * scale) / 2.0 - y0 * scale;
        self.view.set((scale, ox, oy));
        let to_screen =
            |x: f64, y: f64, w: f64, h: f64| (ox + x * scale, oy + y * scale, w * scale, h * scale);

        // This desktop: each monitor.
        let monitors = self.monitors.borrow().clone();
        let monitors = if monitors.is_empty() {
            vec![(0.0, 0.0, mw, mh)]
        } else {
            monitors
        };
        for (x, y, w, h) in &monitors {
            let (sx, sy, sw, sh) = to_screen(*x, *y, *w, *h);
            rounded(cr, sx + 1.5, sy + 1.5, sw - 3.0, sh - 3.0, 8.0);
            cr.set_source_rgb(0.21, 0.52, 0.89);
            let _ = cr.fill();
        }
        let (sx, sy, sw, sh) = to_screen(0.0, 0.0, mw, mh);
        let label = if monitors.len() > 1 {
            format!("This computer · {} screens", monitors.len())
        } else {
            "This computer".to_owned()
        };
        centered_text(cr, &label, sx + sw / 2.0, sy + sh / 2.0, sw - 12.0, true);

        // The online computers around it.
        let control = self.status.borrow().1;
        let dragging = self.drag.borrow().map(|d| d.id);
        for (p, (x, y, w, h)) in &peers {
            let (sx, sy, sw, sh) = to_screen(*x, *y, *w, *h);
            let active =
                matches!(control, Control::Controlling(c) | Control::ControlledBy(c) if c == p.id);
            rounded(cr, sx + 1.5, sy + 1.5, sw - 3.0, sh - 3.0, 8.0);
            if active {
                cr.set_source_rgb(0.85, 0.55, 0.1);
            } else if dragging == Some(p.id) {
                cr.set_source_rgb(0.3, 0.68, 0.45);
            } else {
                cr.set_source_rgb(0.18, 0.55, 0.34);
            }
            let _ = cr.fill();
            centered_text(cr, &p.name, sx + sw / 2.0, sy + sh / 2.0, sw - 12.0, false);
            // Where the cursor crosses: the stretch of edge the two share.
            let size = self.peer_sizes.borrow().get(&p.id).copied();
            let span = layout::Span::new(Some(mine), Some(size.unwrap_or(mine)), p.edge, p.offset);
            if dragging != Some(p.id)
                && let Some((a, b)) = span.touching()
            {
                let (a, b) = (f64::from(a), f64::from(b));
                let (dx0, dy0, dw, dh) = to_screen(0.0, 0.0, mw, mh);
                let (x_a, y_a, x_b, y_b) = match p.edge {
                    Edge::Right => (dx0 + dw, dy0 + a * dh, dx0 + dw, dy0 + b * dh),
                    Edge::Left => (dx0, dy0 + a * dh, dx0, dy0 + b * dh),
                    Edge::Top => (dx0 + a * dw, dy0, dx0 + b * dw, dy0),
                    Edge::Bottom => (dx0 + a * dw, dy0 + dh, dx0 + b * dw, dy0 + dh),
                };
                cr.set_source_rgb(1.0, 1.0, 1.0);
                cr.set_line_width(4.0);
                cr.move_to(x_a, y_a);
                cr.line_to(x_b, y_b);
                let _ = cr.stroke();
            }
        }
    }

    fn to_world(&self, x: f64, y: f64) -> (f64, f64) {
        let (scale, ox, oy) = self.view.get();
        ((x - ox) / scale, (y - oy) / scale)
    }

    fn drag_begin(&self, x: f64, y: f64) {
        let (wx, wy) = self.to_world(x, y);
        let hit = self
            .peer_rects()
            .into_iter()
            .find(|(_, (rx, ry, rw, rh))| wx >= *rx && wx <= rx + rw && wy >= *ry && wy <= ry + rh);
        *self.drag.borrow_mut() = hit.map(|(p, (rx, ry, rw, rh))| Drag {
            id: p.id,
            x0: rx,
            y0: ry,
            x: rx,
            y: ry,
            w: rw,
            h: rh,
        });
    }

    fn drag_update(&self, dx: f64, dy: f64) {
        let (scale, _, _) = self.view.get();
        if let Some(d) = self.drag.borrow_mut().as_mut() {
            d.x = d.x0 + dx / scale;
            d.y = d.y0 + dy / scale;
        }
        self.canvas.queue_draw();
    }

    /// Drops the dragged computer on the nearest edge, where it was let go.
    fn drag_end(&self, dx: f64, dy: f64) {
        let Some(d) = self.drag.borrow_mut().take() else {
            return;
        };
        self.canvas.queue_draw();
        if dx.abs() + dy.abs() < 4.0 {
            return; // A click, not a drag.
        }
        let mine = self.my_size.get().unwrap_or(DEFAULT_SIZE);
        let (edge, offset) = layout::snap(mine, (d.x, d.y, d.w, d.h));
        let _ = self.cmd.send(Command::SetPlacement {
            id: d.id,
            edge,
            offset,
        });
    }
}

/// A rounded rectangle path.
fn rounded(cr: &gtk::cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    let pi = std::f64::consts::PI;
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -pi / 2.0, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, pi / 2.0);
    cr.arc(x + r, y + h - r, r, pi / 2.0, pi);
    cr.arc(x + r, y + r, r, pi, 1.5 * pi);
    cr.close_path();
}

/// White text centred at (cx, cy), shrunk to fit `max_w`.
fn centered_text(cr: &gtk::cairo::Context, text: &str, cx: f64, cy: f64, max_w: f64, bold: bool) {
    cr.select_font_face(
        "Sans",
        gtk::cairo::FontSlant::Normal,
        if bold {
            gtk::cairo::FontWeight::Bold
        } else {
            gtk::cairo::FontWeight::Normal
        },
    );
    let mut size = 14.0;
    cr.set_font_size(size);
    if let Ok(e) = cr.text_extents(text)
        && e.width() > max_w
        && e.width() > 0.0
    {
        size = (size * max_w / e.width()).max(7.0);
        cr.set_font_size(size);
    }
    if let Ok(e) = cr.text_extents(text) {
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.move_to(
            cx - e.width() / 2.0 - e.x_bearing(),
            cy - e.height() / 2.0 - e.y_bearing(),
        );
        let _ = cr.show_text(text);
    }
}
