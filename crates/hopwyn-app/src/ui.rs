//! The Hopwyn window (GTK 4 + libadwaita).

use std::cell::{Cell, RefCell};
use std::net::IpAddr;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use hopwyn_proto::Edge;
use tokio::sync::mpsc::UnboundedSender;

use crate::config::Partner;
use crate::service::{self, Command, Control, Link, Nearby, UiEvent};

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

pub fn run() -> glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(|app| {
        if let Some(win) = app.active_window() {
            win.present();
            return;
        }
        build(app);
    });
    app.run()
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
    partner_row: adw::ActionRow,
    side_row: adw::ComboRow,
    forget_button: gtk::Button,
    empty_row: adw::ActionRow,
    nearby_group: adw::PreferencesGroup,
    nearby_empty: adw::ActionRow,
    nearby_rows: RefCell<Vec<adw::ActionRow>>,
    code_dialog: RefCell<Option<adw::AlertDialog>>,
    cmd: UnboundedSender<Command>,
    partner: RefCell<Option<Partner>>,
    status: RefCell<(Link, Control)>,
    ready: Cell<bool>,
    updating_side: Cell<bool>,
}

fn build(app: &adw::Application) {
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

    // Paired device.
    let partner_group = adw::PreferencesGroup::builder()
        .title("Paired computer")
        .build();
    let partner_row = adw::ActionRow::builder().title("—").build();
    partner_row.add_prefix(&gtk::Image::from_icon_name("computer-symbolic"));
    let forget_button = gtk::Button::builder()
        .label("Forget")
        .valign(gtk::Align::Center)
        .css_classes(["destructive-action", "flat"])
        .build();
    partner_row.add_suffix(&forget_button);
    let side_row = adw::ComboRow::builder()
        .title("Its screen is")
        .model(&gtk::StringList::new(&SIDES))
        .build();
    let empty_row = adw::ActionRow::builder()
        .title("No computer paired yet")
        .subtitle("Pair one under Nearby, or show a code with Add computer")
        .build();
    partner_group.add(&empty_row);

    // Add a device.
    let add_group = adw::PreferencesGroup::builder()
        .title("Add a computer")
        .description("Open Hopwyn on the other computer too. One shows a code, the other types it.")
        .build();
    let add_row = adw::ActionRow::builder()
        .title("Show a pairing code")
        .subtitle("Valid for 2 minutes")
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
    tips.add(&tip_panic);
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
        partner_row,
        side_row,
        forget_button,
        empty_row,
        nearby_group,
        nearby_empty,
        nearby_rows: RefCell::new(Vec::new()),
        code_dialog: RefCell::new(None),
        cmd,
        partner: RefCell::new(None),
        status: RefCell::new((Link::NoPartner, Control::Local)),
        ready: Cell::new(false),
        updating_side: Cell::new(false),
    });

    {
        let ui2 = ui.clone();
        add_button.connect_clicked(move |_| ui2.show_code_dialog());
    }
    {
        let ui2 = ui.clone();
        ui.forget_button
            .connect_clicked(move |_| ui2.confirm_forget());
    }
    {
        let ui2 = ui.clone();
        ui.side_row.connect_selected_notify(move |row| {
            if ui2.updating_side.get() {
                return;
            }
            if let Some(edge) = index_edge(row.selected()) {
                let _ = ui2.cmd.send(Command::SetEdge(edge));
            }
        });
    }
    {
        let ui2 = ui.clone();
        window.connect_close_request(move |_| {
            let _ = ui2.cmd.send(Command::Quit);
            glib::Propagation::Proceed
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

    window.present();
}

impl Ui {
    fn handle(self: &Rc<Self>, ev: UiEvent) {
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
            UiEvent::Status { link, control } => {
                *self.status.borrow_mut() = (link, control);
                self.refresh_card();
            }
            UiEvent::Partner(p) => {
                *self.partner.borrow_mut() = p;
                self.refresh_partner();
                self.refresh_card();
            }
            UiEvent::Nearby(list) => self.refresh_nearby(&list),
            UiEvent::PairingCode(code) => self.show_code(&code),
            UiEvent::PairingDone(name) => {
                self.close_code_dialog();
                self.toast(&format!("Paired with {name}"));
            }
            UiEvent::PairingFailed(why) => {
                self.close_code_dialog();
                self.toast(&format!("Pairing failed: {why}"));
            }
            UiEvent::Error(e) => {
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

    fn refresh_card(&self) {
        if !self.ready.get() {
            return;
        }
        let partner = self.partner.borrow();
        let (link, control) = self.status.borrow().clone();
        let Some(p) = partner.as_ref() else {
            self.set_card(
                "computer-symbolic",
                "No computer paired",
                "Pair a computer below to share this mouse and keyboard with it.",
                "",
            );
            return;
        };
        match (link, control) {
            (Link::Connected, Control::ControllingPartner) => self.set_card(
                "go-next-symbolic",
                &format!("Controlling {}", p.name),
                "This computer is inactive while its mouse and keyboard drive the other one.",
                "inactive",
            ),
            (Link::Connected, Control::ControlledByPartner) => self.set_card(
                "go-previous-symbolic",
                &format!("Controlled by {}", p.name),
                &format!("{}'s mouse and keyboard are driving this computer.", p.name),
                "",
            ),
            (Link::Connected, Control::Local) => self.set_card(
                "input-mouse-symbolic",
                "Ready",
                &format!(
                    "Move the cursor off the {} edge to use {}.",
                    edge_words(p.edge),
                    p.name
                ),
                "",
            ),
            _ => self.set_card(
                "network-wireless-symbolic",
                &format!("Looking for {}", p.name),
                "Make sure Hopwyn is open on it and both are on the same network.",
                "",
            ),
        }
        drop(partner);
        self.refresh_partner_subtitle();
    }

    fn refresh_partner_subtitle(&self) {
        let partner = self.partner.borrow();
        if let Some(p) = partner.as_ref() {
            let state = match self.status.borrow().0 {
                Link::Connected => "Connected",
                _ => "Not connected",
            };
            self.partner_row
                .set_subtitle(&format!("{state} · {} side", edge_words(p.edge)));
        }
    }

    fn refresh_partner(&self) {
        let partner = self.partner.borrow().clone();
        // Rebuild the group's rows for the current state.
        for row in [
            self.empty_row.upcast_ref::<gtk::Widget>(),
            self.partner_row.upcast_ref(),
            self.side_row.upcast_ref(),
        ] {
            if row.parent().is_some() {
                self.partner_group.remove(row);
            }
        }
        match partner {
            Some(p) => {
                self.partner_row.set_title(&p.name);
                self.updating_side.set(true);
                self.side_row.set_selected(edge_index(p.edge));
                self.updating_side.set(false);
                self.partner_group.add(&self.partner_row);
                self.partner_group.add(&self.side_row);
                self.refresh_partner_subtitle();
            }
            None => self.partner_group.add(&self.empty_row),
        }
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
            let row = adw::ActionRow::builder()
                .title(&n.name)
                .subtitle(n.ip.to_string())
                .build();
            row.add_prefix(&gtk::Image::from_icon_name("computer-symbolic"));
            if n.paired {
                let tag = gtk::Label::builder()
                    .label("Paired")
                    .css_classes(["dim-label"])
                    .build();
                row.add_suffix(&tag);
            } else {
                let button = gtk::Button::builder()
                    .label("Pair…")
                    .valign(gtk::Align::Center)
                    .build();
                let ui = self.clone();
                let (name, ip) = (n.name.clone(), n.ip);
                button.connect_clicked(move |_| ui.show_join_dialog(&name, ip));
                row.add_suffix(&button);
                row.set_activatable_widget(Some(&button));
            }
            self.nearby_group.add(&row);
            self.nearby_rows.borrow_mut().push(row);
        }
    }

    fn show_code_dialog(self: &Rc<Self>) {
        if self.partner.borrow().is_some() {
            let dialog = adw::AlertDialog::new(
                Some("Replace the paired computer?"),
                Some(
                    "Hopwyn shares with one computer at a time. Pairing a new one replaces the current one.",
                ),
            );
            dialog.add_response("cancel", "Cancel");
            dialog.add_response("continue", "Continue");
            dialog.set_close_response("cancel");
            let ui = self.clone();
            dialog.connect_response(None, move |_, response| {
                if response == "continue" {
                    let _ = ui.cmd.send(Command::AddDevice);
                }
            });
            dialog.present(Some(&self.window));
            return;
        }
        let _ = self.cmd.send(Command::AddDevice);
    }

    fn show_code(self: &Rc<Self>, code: &str) {
        self.close_code_dialog();
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
            Some("Pairing code"),
            Some(
                "On the other computer, open Hopwyn, choose this computer under Nearby and type this code. It works for 2 minutes.",
            ),
        );
        dialog.set_extra_child(Some(&label));
        dialog.add_response("cancel", "Cancel");
        dialog.set_close_response("cancel");
        let ui = self.clone();
        dialog.connect_response(None, move |_, response| {
            if response == "cancel" {
                ui.code_dialog.borrow_mut().take();
                let _ = ui.cmd.send(Command::CancelPairing);
            }
        });
        dialog.present(Some(&self.window));
        *self.code_dialog.borrow_mut() = Some(dialog);
    }

    fn close_code_dialog(&self) {
        if let Some(d) = self.code_dialog.borrow_mut().take() {
            d.force_close();
        }
    }

    fn show_join_dialog(self: &Rc<Self>, name: &str, ip: IpAddr) {
        let entry = gtk::Entry::builder()
            .placeholder_text("123 456")
            .input_purpose(gtk::InputPurpose::Digits)
            .max_length(7)
            .activates_default(true)
            .build();
        let dialog = adw::AlertDialog::new(
            Some(&format!("Pair with {name}")),
            Some(&format!(
                "On {name}, click Add computer, then type the code it shows."
            )),
        );
        dialog.set_extra_child(Some(&entry));
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("pair", "Pair");
        dialog.set_response_appearance("pair", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("pair"));
        dialog.set_close_response("cancel");
        let ui = self.clone();
        let name = name.to_owned();
        let code_entry = entry.clone();
        dialog.connect_response(None, move |_, response| {
            if response == "pair" {
                let code: String = code_entry
                    .text()
                    .chars()
                    .filter(char::is_ascii_digit)
                    .collect();
                ui.toast(&format!("Pairing with {name}…"));
                let _ = ui.cmd.send(Command::JoinDevice { ip, code });
            }
        });
        dialog.present(Some(&self.window));
        entry.grab_focus();
    }

    fn confirm_forget(self: &Rc<Self>) {
        let name = self
            .partner
            .borrow()
            .as_ref()
            .map(|p| p.name.clone())
            .unwrap_or_default();
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
                let _ = ui.cmd.send(Command::Forget);
            }
        });
        dialog.present(Some(&self.window));
    }
}
