//! The Hopwyn window (GTK 4 + libadwaita).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use hopwyn_proto::Edge;
use tokio::sync::mpsc::UnboundedSender;

use crate::config::MAX_PARTNERS;
use crate::config::Partner;
use crate::service::{self, Command, Control, Nearby, UiEvent};
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
.layout-tile { padding: 10px 12px; border-radius: 10px; min-width: 96px; min-height: 52px; }
.layout-tile.this { background: alpha(@accent_bg_color, 0.85); color: @accent_fg_color; font-weight: 700; }
.layout-tile.peer { background: alpha(@card_fg_color, 0.08); }
.layout-tile.peer.online { background: alpha(@success_color, 0.18); }
.layout-tile.peer.active { background: alpha(@warning_color, 0.30); }
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

/// `--background` starts Hopwyn without showing its window (used at login).
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
    // permissions stay alive and the dialogs are not asked again.
    let _hold = app.hold();
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
    layout: gtk::Grid,
    /// Left, right, top, bottom.
    layout_tiles: [gtk::Label; 4],
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

    // Paired computers, with a picture of where each one sits.
    let partner_group = adw::PreferencesGroup::builder()
        .title("Paired computers")
        .description(format!(
            "Up to {MAX_PARTNERS}, one on each side of this screen. Pick the side each one is on."
        ))
        .build();
    let layout = gtk::Grid::builder()
        .row_spacing(8)
        .column_spacing(8)
        .halign(gtk::Align::Center)
        .margin_bottom(12)
        .build();
    let tile = |text: &str, classes: &[&str]| {
        let l = gtk::Label::builder()
            .label(text)
            .wrap(true)
            .justify(gtk::Justification::Center)
            .max_width_chars(12)
            .build();
        l.add_css_class("layout-tile");
        for c in classes {
            l.add_css_class(c);
        }
        l
    };
    layout.attach(&tile("This computer", &["this"]), 1, 1, 1, 1);
    let layout_tiles: [gtk::Label; 4] = [
        tile("", &["peer"]),
        tile("", &["peer"]),
        tile("", &["peer"]),
        tile("", &["peer"]),
    ];
    for (edge, t) in [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom]
        .into_iter()
        .zip(layout_tiles.iter())
    {
        let (col, row) = match edge {
            Edge::Left => (0, 1),
            Edge::Right => (2, 1),
            Edge::Top => (1, 0),
            Edge::Bottom => (1, 2),
        };
        layout.attach(t, col, row, 1, 1);
    }
    layout.set_visible(false);
    partner_group.add(&layout);
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
        layout,
        layout_tiles,
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

    window
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
        for (edge, tile) in [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom]
            .into_iter()
            .zip(self.layout_tiles.iter())
        {
            let p = partners.iter().find(|p| p.edge == edge);
            tile.set_visible(p.is_some());
            tile.remove_css_class("online");
            tile.remove_css_class("active");
            if let Some(p) = p {
                tile.set_label(&p.name);
                let active = matches!(control, Control::Controlling(c) | Control::ControlledBy(c) if c == p.id);
                if active {
                    tile.add_css_class("active");
                } else if connected.contains(&p.id) {
                    tile.add_css_class("online");
                }
            }
        }
        self.layout.set_visible(!partners.is_empty());
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
            row.add_suffix(&side);
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
