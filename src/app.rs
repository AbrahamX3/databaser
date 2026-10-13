//! The Databaser window.
//!
//! Layout follows a restrained "glass plate in a tray" motif: every card is a
//! soft outer shell (a faint tint plus a hairline) wrapping a raised inner
//! surface, with generous spacing and a single, quiet menu for all settings.

use std::collections::HashMap;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::base::Disableable as _;
use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::description_list::{DescriptionItem, DescriptionList};
use gpui_kit::component::link::Link;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, Side, Theme, ThemeMode, WindowExt as _,
};
use gpui_kit::*;

use crate::assets::APP_ICON;
use crate::autostart;
use crate::platform;
use crate::tray;
use crate::elevation::{self, ActionOutcome, ControlMode};
use crate::helper_task::{self, TaskError};
use crate::services::{self, ServiceAction, ServiceInfo, ServiceKind, ServiceState, StartType};
use crate::settings::{Settings, ThemePreference};
use crate::update;

/// How often the service states are re-read while the window is open.
const POLL_INTERVAL: Duration = Duration::from_millis(2000);

/// Delay before the automatic launch update check, so start-up stays snappy.
const AUTO_CHECK_DELAY: Duration = Duration::from_secs(6);

/// Repository URL, taken from `Cargo.toml` so it cannot drift from the updater's
/// own `REPO_OWNER` / `REPO_NAME` (a test below keeps the two in step).
const REPOSITORY_URL: &str = env!("CARGO_PKG_REPOSITORY");

#[derive(Clone, Copy)]
enum ToastKind {
    Success,
    Warning,
    Error,
}

/// Progress of an in-app update check / install.
#[derive(Debug, Default)]
enum UpdateState {
    #[default]
    Idle,
    Checking,
    Available(String),
    Installing(String),
    ReadyToRestart(String),
}

/// Listening ports for a service, fetched the first time its popover is opened
/// rather than on every poll.
#[derive(Debug, Clone)]
enum Ports {
    Loading,
    Ready(Vec<u16>),
    /// The service is not running, or the socket table could not be read.
    Unavailable,
}

pub struct Databaser {
    services: Vec<ServiceInfo>,
    /// Listening ports per service name, filled lazily from the popover.
    ports: HashMap<String, Ports>,
    /// Name of the service currently being acted on, if any.
    busy: Option<String>,
    busy_action: Option<ServiceAction>,
    scanning: bool,
    /// True while the no-prompt scheduled task is being installed or removed,
    /// so the toggle cannot be clicked twice while UAC is up.
    elevation_busy: bool,
    /// How privileged actions are performed (probed on start-up).
    mode: ControlMode,
    /// A persistent problem (e.g. discovery failed), shown as a banner.
    error: Option<String>,
    theme: ThemePreference,
    settings: Settings,
    toasts: Vec<(ToastKind, String)>,
    update: UpdateState,
    /// Whether the per-user Run entry points at this executable. Read from the
    /// registry at start-up and after each toggle, never persisted here.
    autostart: bool,
}

impl Databaser {
    pub fn new(settings: Settings, cx: &mut Context<Self>) -> Self {
        let this = Self {
            services: Vec::new(),
            ports: HashMap::new(),
            busy: None,
            busy_action: None,
            scanning: true,
            elevation_busy: false,
            mode: ControlMode::Uac,
            error: None,
            theme: settings.theme,
            settings,
            toasts: Vec::new(),
            update: UpdateState::Idle,
            autostart: autostart::is_enabled(),
        };

        // Discovery, the elevation-mode probe, and state polling all touch
        // blocking Windows APIs, so they run on the background executor and
        // only publish results to the UI.
        Self::spawn_discovery(cx);
        Self::spawn_mode_probe(cx);
        Self::spawn_poll(cx);
        Self::spawn_auto_update_check(cx);

        this
    }

    // ---- data -----------------------------------------------------------

    fn spawn_discovery(cx: &mut Context<Self>) {
        cx.spawn(async move |entity, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { services::discover() })
                .await;

            entity
                .update(cx, |this, cx| {
                    this.scanning = false;
                    match result {
                        Ok(services) => {
                            this.services = services;
                            this.error = None;
                        }
                        Err(error) => this.error = Some(error.to_string()),
                    }
                    cx.notify();
                })
                .ok();
        })
        .detach();
    }

    fn spawn_mode_probe(cx: &mut Context<Self>) {
        cx.spawn(async move |entity, cx| {
            let mode = cx
                .background_executor()
                .spawn(async { elevation::detect_mode() })
                .await;
            entity
                .update(cx, |this, cx| {
                    this.mode = mode;
                    cx.notify();
                })
                .ok();
        })
        .detach();
    }

    fn rescan(&mut self, cx: &mut Context<Self>) {
        if self.scanning {
            return;
        }
        self.scanning = true;
        cx.notify();
        Self::spawn_discovery(cx);
    }

    /// Re-read service states off the UI thread.
    fn spawn_poll(cx: &mut Context<Self>) {
        cx.spawn(async move |entity, cx| {
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;

                let names = match entity.update(cx, |this, _| {
                    if this.busy.is_some() {
                        return None;
                    }
                    Some(this.services.iter().map(|s| s.name.clone()).collect::<Vec<_>>())
                }) {
                    Ok(names) => names,
                    Err(_) => break,
                };

                let Some(names) = names else { continue };
                if names.is_empty() {
                    continue;
                }

                let states = cx
                    .background_executor()
                    .spawn(async move { services::query_states(&names) })
                    .await;

                let applied = entity.update(cx, |this, cx| {
                    let Ok(states) = states else { return };
                    for (info, state) in this.services.iter_mut().zip(states) {
                        if let Some(state) = state {
                            info.state = state;
                        }
                    }
                    cx.notify();
                });
                if applied.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    // ---- privileged actions ---------------------------------------------

    fn setup_elevated(&mut self, cx: &mut Context<Self>) {
        if self.elevation_busy {
            return;
        }
        self.elevation_busy = true;
        cx.notify();

        cx.spawn(async move |entity, cx| {
            let result = cx
                .background_executor()
                .spawn(async {
                    let exe = std::env::current_exe().map_err(|error| {
                        TaskError::Setup(format!("could not locate the application: {error}"))
                    })?;
                    helper_task::setup(&exe)
                })
                .await;

            entity
                .update(cx, |this, cx| {
                    this.elevation_busy = false;
                    match result {
                        Ok(()) => {
                            this.mode = ControlMode::Task;
                            this.push_toast(
                                ToastKind::Success,
                                "No-prompt control is on \u{2014} Start/Stop no longer prompt."
                                    .to_string(),
                            );
                        }
                        Err(TaskError::Cancelled) => this.push_toast(
                            ToastKind::Warning,
                            "Setup cancelled \u{2014} administrator rights are needed once."
                                .to_string(),
                        ),
                        Err(error) => this.push_toast(ToastKind::Error, error.to_string()),
                    }
                    cx.notify();
                })
                .ok();
        })
        .detach();
    }

    fn remove_elevated(&mut self, cx: &mut Context<Self>) {
        if self.elevation_busy {
            return;
        }
        self.elevation_busy = true;
        cx.notify();

        cx.spawn(async move |entity, cx| {
            let result = cx
                .background_executor()
                .spawn(async { helper_task::remove() })
                .await;

            entity
                .update(cx, |this, cx| {
                    this.elevation_busy = false;
                    match result {
                        Ok(()) => {
                            this.mode = ControlMode::Uac;
                            this.push_toast(
                                ToastKind::Success,
                                "No-prompt control is off \u{2014} Start/Stop will prompt again."
                                    .to_string(),
                            );
                        }
                        Err(TaskError::RemoveCancelled) => this.push_toast(
                            ToastKind::Warning,
                            "Removal cancelled \u{2014} administrator rights are needed to remove it."
                                .to_string(),
                        ),
                        Err(error) => this.push_toast(ToastKind::Error, error.to_string()),
                    }
                    cx.notify();
                })
                .ok();
        })
        .detach();
    }

    fn request_action(&mut self, name: String, action: ServiceAction, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }

        // Windows refuses to start a disabled service and the SCM reports it as
        // the opaque "cannot accept the request in its current state". Say what
        // is actually wrong, and where the fix is.
        if action == ServiceAction::Start
            && self
                .services
                .iter()
                .any(|service| service.name == name && service.start_type == StartType::Disabled)
        {
            self.push_toast(
                ToastKind::Warning,
                format!(
                    "`{name}` is disabled, so Windows will not start it. \
                     Enable it from Details \u{2192} Start-up."
                ),
            );
            cx.notify();
            return;
        }

        let mode = self.mode;
        self.busy = Some(name.clone());
        self.busy_action = Some(action);
        cx.notify();

        cx.spawn(async move |entity, cx| {
            let action_name = name.clone();
            let outcome = cx
                .background_executor()
                .spawn(async move { elevation::perform(mode, action, &action_name) })
                .await;

            entity
                .update(cx, |this, cx| {
                    this.busy = None;
                    this.busy_action = None;

                    match outcome {
                        ActionOutcome::Done => {
                            // Reflect the new start type immediately; the poll
                            // only re-reads states, not configuration.
                            if let ServiceAction::SetStartType(start_type) = action {
                                let running =
                                    match this.services.iter_mut().find(|s| s.name == name) {
                                        Some(service) => {
                                            service.start_type = start_type;
                                            service.state.is_running()
                                        }
                                        None => false,
                                    };
                                this.push_toast(
                                    ToastKind::Success,
                                    start_type_message(&name, start_type, running),
                                );
                            } else {
                                this.push_toast(
                                    ToastKind::Success,
                                    format!("{} {}", action.done_label(), name),
                                );
                            }
                        }
                        ActionOutcome::Cancelled => this.push_toast(
                            ToastKind::Warning,
                            format!(
                                "{} cancelled \u{2014} administrator rights are required.",
                                action.ing_label()
                            ),
                        ),
                        ActionOutcome::Failed(message) => {
                            this.push_toast(ToastKind::Error, message)
                        }
                    }
                    cx.notify();
                })
                .ok();
        })
        .detach();
    }

    fn push_toast(&mut self, kind: ToastKind, message: String) {
        self.toasts.push((kind, message));
    }

    /// Look up which ports a service is listening on, once, on first use.
    ///
    /// Both the SCM query and the socket table are blocking, so this runs on the
    /// background executor like everything else that touches Windows.
    fn fetch_ports(&mut self, name: String, cx: &mut Context<Self>) {
        if self.ports.contains_key(&name) {
            return;
        }
        self.ports.insert(name.clone(), Ports::Loading);

        cx.spawn(async move |entity, cx| {
            let query_name = name.clone();
            let found = cx
                .background_executor()
                .spawn(async move {
                    services::process_id(&query_name)
                        .map(services::listening_ports)
                        .transpose()
                })
                .await;

            entity
                .update(cx, |this, cx| {
                    let state = match found {
                        Ok(Some(ports)) => Ports::Ready(ports),
                        // Not running, or the socket table could not be read.
                        Ok(None) | Err(_) => Ports::Unavailable,
                    };
                    this.ports.insert(name, state);
                    cx.notify();
                })
                .ok();
        })
        .detach();
    }

    /// Turn the tray icon, and with it "closing hides the window", on or off.
    fn toggle_background(&mut self, cx: &mut Context<Self>) {
        let wanted = !self.settings.background;

        if wanted {
            if let Err(error) = tray::install(cx) {
                self.push_toast(
                    ToastKind::Error,
                    format!("Could not create the tray icon: {error}"),
                );
                cx.notify();
                return;
            }
        } else {
            tray::remove(cx);
        }

        self.settings.background = wanted;
        self.settings.save();
        self.push_toast(
            ToastKind::Success,
            if wanted {
                "Run in background is on \u{2014} closing the window keeps Databaser in the tray."
            } else {
                "Run in background is off \u{2014} closing the window exits."
            }
            .to_string(),
        );
        cx.notify();
    }

    /// Switch the per-user `Run` entry on or off.
    ///
    /// No elevation: the key lives under `HKCU`.
    fn toggle_autostart(&mut self, cx: &mut Context<Self>) {
        let wanted = !self.autostart;
        match autostart::set(wanted) {
            Ok(()) => {
                self.autostart = wanted;
                self.push_toast(
                    ToastKind::Success,
                    if wanted {
                        "Start with Windows is on \u{2014} Databaser opens when you sign in."
                    } else {
                        "Start with Windows is off."
                    }
                    .to_string(),
                );
            }
            Err(error) => self.push_toast(
                ToastKind::Error,
                format!("Could not change the Windows start-up entry: {error}"),
            ),
        }
        cx.notify();
    }

    // ---- updates ---------------------------------------------------------

    /// Ask GitHub Releases whether a newer version exists.
    ///
    /// `manual` is the Menu action: it always reports a result (including "up to
    /// date" and errors). The automatic launch check is cached, stays silent on
    /// failure, and respects a dismissed version.
    fn check_updates(&mut self, cx: &mut Context<Self>, manual: bool) {
        if matches!(self.update, UpdateState::Checking | UpdateState::Installing(_)) {
            return;
        }
        self.update = UpdateState::Checking;
        cx.notify();

        let skipped = self.settings.skipped_version.clone();

        cx.spawn(async move |entity, cx| {
            let result = cx
                .background_executor()
                .spawn(async { update::check() })
                .await;

            entity
                .update(cx, |this, cx| {
                    this.settings.record_update_check();

                    match result {
                        Ok(Some(version)) => {
                            // Remember what we found, so a later launch inside
                            // the throttle window can show it without asking
                            // GitHub again.
                            this.settings.available_version = Some(version.clone());
                            this.update =
                                if should_offer_update(manual, &version, skipped.as_deref()) {
                                    UpdateState::Available(version)
                                } else {
                                    UpdateState::Idle
                                };
                        }
                        Ok(None) => {
                            this.settings.available_version = None;
                            this.update = UpdateState::Idle;
                            if manual {
                                this.push_toast(
                                    ToastKind::Success,
                                    format!(
                                        "Databaser v{} is the latest version.",
                                        env!("CARGO_PKG_VERSION")
                                    ),
                                );
                            }
                        }
                        Err(error) => {
                            // A failed check learns nothing, so leave whatever
                            // was cached from the last successful one.
                            this.update = UpdateState::Idle;
                            if manual {
                                this.push_toast(ToastKind::Error, error.to_string());
                            }
                        }
                    }

                    this.settings.save();
                    cx.notify();
                })
                .ok();
        })
        .detach();
    }

    /// Check once shortly after launch, at most once a day.
    fn spawn_auto_update_check(cx: &mut Context<Self>) {
        cx.spawn(async move |entity, cx| {
            // Let service discovery and the first frames settle first.
            cx.background_executor().timer(AUTO_CHECK_DELAY).await;

            entity
                .update(cx, |this, cx| {
                    if this.settings.update_check_due() {
                        this.check_updates(cx, false);
                        return;
                    }

                    // Inside the throttle window: no network call, but still
                    // show an update a previous check already found. Without
                    // this, closing and reopening the app hid a known update
                    // until the next check came due.
                    let Some(cached) = this.settings.available_version.clone() else {
                        return;
                    };
                    let skipped = this.settings.skipped_version.clone();
                    if is_newer(&cached, env!("CARGO_PKG_VERSION"))
                        && should_offer_update(false, &cached, skipped.as_deref())
                    {
                        this.update = UpdateState::Available(cached);
                        cx.notify();
                    }
                })
                .ok();
        })
        .detach();
    }

    /// Remember the dismissed version so the banner does not come back for it.
    fn dismiss_update(&mut self, version: String, cx: &mut Context<Self>) {
        self.settings.skipped_version = Some(version);
        self.settings.save();
        self.update = UpdateState::Idle;
        cx.notify();
    }

    /// Download and install the available update (off the UI thread).
    ///
    /// The running process keeps using the old binary until it restarts, so we
    /// surface a "Restart" step rather than relaunching underneath the user.
    fn install_update(&mut self, cx: &mut Context<Self>) {
        let UpdateState::Available(version) = &self.update else {
            return;
        };
        self.update = UpdateState::Installing(version.clone());
        cx.notify();

        cx.spawn(async move |entity, cx| {
            let result = cx
                .background_executor()
                .spawn(async { update::install() })
                .await;

            entity
                .update(cx, |this, cx| {
                    match result {
                        Ok(Some(version)) => {
                            this.push_toast(
                                ToastKind::Success,
                                format!("Updated to v{version}. Restart to finish."),
                            );
                            this.update = UpdateState::ReadyToRestart(version);
                        }
                        Ok(None) => {
                            this.update = UpdateState::Idle;
                            this.push_toast(
                                ToastKind::Success,
                                "Already up to date.".to_string(),
                            );
                        }
                        Err(error) => {
                            this.update = UpdateState::Idle;
                            this.push_toast(ToastKind::Error, error.to_string());
                        }
                    }
                    cx.notify();
                })
                .ok();
        })
        .detach();
    }

    /// The update banner, if there is anything to say about updates.
    fn update_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let row = match &self.update {
            UpdateState::Idle | UpdateState::Checking => return None,
            UpdateState::Available(version) => {
                let text = format!("Databaser v{version} is available.");
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .child(icon(IconName::Download))
                    .child(div().flex_grow(1.0).text_sm().child(text))
                    .child(
                        Button::new("update-install")
                            .small()
                            .primary()
                            .label("Install and restart")
                            .on_click(cx.listener(|this, _, _window, cx| this.install_update(cx))),
                    )
                    .child(
                        Button::new("update-dismiss")
                            .ghost()
                            .small()
                            .icon(icon(IconName::X))
                            .tooltip("Skip this version")
                            .on_click({
                                let version = version.clone();
                                cx.listener(move |this, _, _window, cx| {
                                    this.dismiss_update(version.clone(), cx)
                                })
                            }),
                    )
            }
            UpdateState::Installing(version) => {
                let text = format!("Installing v{version}\u{2026}");
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .child(Spinner::new().small())
                    .child(div().flex_grow(1.0).text_sm().child(text))
            }
            UpdateState::ReadyToRestart(version) => {
                let text = format!("Updated to v{version}. Restart to finish.");
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .child(icon(IconName::Download))
                    .child(div().flex_grow(1.0).text_sm().child(text))
                    .child(
                        Button::new("update-restart")
                            .small()
                            .primary()
                            .label("Restart")
                            .on_click(cx.listener(|this, _, _window, cx| this.restart_app(cx))),
                    )
            }
        };

        Some(
            div()
                .px(px(28.))
                .pt(px(20.))
                .child(
                    row.px(px(18.))
                        .py(px(14.))
                        .rounded_lg()
                        .bg(cx.theme().secondary)
                        .border_1()
                        .border_color(cx.theme().border),
                )
                .into_any_element(),
        )
    }

    /// Relaunch with the freshly installed binary.
    fn restart_app(&mut self, cx: &mut Context<Self>) {
        cx.restart();
    }

    fn set_theme(&mut self, pref: ThemePreference, window: &mut Window, cx: &mut Context<Self>) {
        if self.theme == pref {
            return;
        }
        self.theme = pref;
        self.settings.theme = pref;
        self.settings.save();
        apply_theme(pref, Some(window), cx);
        cx.notify();
    }

    // ---- chrome ----------------------------------------------------------

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap_3()
            .px(px(28.))
            .py(px(20.))
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .flex_grow(1.0)
                    .flex_shrink(1.0)
                    .text_xl()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Databaser"),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_shrink(0.0)
                    .items_center()
                    .gap_3()
                    .child(self.scan_indicator(cx))
                    .child(self.settings_menu(cx)),
            )
    }

    fn scan_indicator(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut row = div().flex().flex_row().items_center().gap_2();
        if self.scanning {
            row = row.child(Spinner::new().small()).child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Scanning\u{2026}"),
            );
        }
        row
    }

    fn settings_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let theme = self.theme;
        let mode = self.mode;
        let elevation_busy = self.elevation_busy;
        let autostart = self.autostart;
        let background = self.settings.background;

        Button::new("settings-menu")
            .icon(icon(IconName::Ellipsis))
            .tooltip("Menu")
            .ghost()
            .small()
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, window, cx| {
                let menu = menu
                    .item(
                        PopupMenuItem::new("Rescan")
                            .icon(icon(IconName::RefreshCw))
                            .on_click({
                                let entity = entity.clone();
                                move |_, _, cx| {
                                    entity.update(cx, Databaser::rescan);
                                }
                            }),
                    )
                    .separator();

                // Ticks go on the right: when an item has an icon, the left
                // slot is taken by the icon and the check would never be drawn.
                let menu = menu
                    .check_side(Side::Right)
                    .item(
                        PopupMenuItem::new("Start with Windows")
                            .icon(icon(IconName::Rocket))
                            .checked(autostart)
                            .on_click({
                                let entity = entity.clone();
                                move |_, _, cx| {
                                    entity.update(cx, Databaser::toggle_autostart);
                                }
                            }),
                    )
                    .item(
                        PopupMenuItem::new("Run in background")
                            .icon(icon(IconName::Monitor))
                            .checked(background)
                            .on_click({
                                let entity = entity.clone();
                                move |_, _, cx| {
                                    entity.update(cx, Databaser::toggle_background);
                                }
                            }),
                    )
                    // One feature with two directions, so one toggle rather than
                    // a submenu offering both at once.
                    .item(
                        PopupMenuItem::new("No-prompt control")
                            .icon(icon(IconName::ShieldCheck))
                            .checked(mode == ControlMode::Task)
                            .disabled(elevation_busy || mode == ControlMode::Elevated)
                            .on_click({
                                let entity = entity.clone();
                                move |_, _, cx| {
                                    entity.update(cx, |this, cx| {
                                        if mode == ControlMode::Task {
                                            this.remove_elevated(cx);
                                        } else {
                                            this.setup_elevated(cx);
                                        }
                                    });
                                }
                            }),
                    )
                    .separator();

                let menu = menu
                    .submenu_with_icon(
                        Some(icon(IconName::Palette)),
                        "Theme",
                        window,
                        cx,
                        {
                            let entity = entity.clone();
                            move |menu, _, _| {
                                // Ticks go on the right so the theme icon can use
                                // the left slot, matching every other item.
                                let menu = menu.check_side(Side::Right);
                                menu.item(theme_item(
                                    "System",
                                    ThemePreference::System,
                                    theme,
                                    IconName::Monitor,
                                    &entity,
                                ))
                                .item(theme_item(
                                    "Light",
                                    ThemePreference::Light,
                                    theme,
                                    IconName::Sun,
                                    &entity,
                                ))
                                .item(theme_item(
                                    "Dark",
                                    ThemePreference::Dark,
                                    theme,
                                    IconName::Moon,
                                    &entity,
                                ))
                            }
                        },
                    )
                    .separator();

                menu.item(
                    PopupMenuItem::new("Check for updates")
                        .icon(icon(IconName::Download))
                        .on_click({
                            let entity = entity.clone();
                            move |_, _, cx| {
                                entity.update(cx, |this, cx| this.check_updates(cx, true));
                            }
                        }),
                )
                .item(
                    PopupMenuItem::new("About")
                        .icon(icon(IconName::Info))
                        .on_click(|_, window, cx| open_about(window, cx)),
                )
            })
    }

    fn error_banner(&self, message: &str) -> impl IntoElement {
        div().px(px(28.)).pt(px(20.)).child(
            Alert::error("discovery-error", message.to_string())
                .title("Something went wrong")
                .banner(),
        )
    }

    fn centered_message(&self, text: &str, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .size_full()
            .items_center()
            .justify_center()
            .gap_4()
            .child(if self.scanning {
                Spinner::new().into_any_element()
            } else {
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(text.to_string())
                    .into_any_element()
            })
            .child(
                Button::new("rescan-empty")
                    .label("Scan again")
                    .outline()
                    .on_click(cx.listener(|this, _, _window, cx| this.rescan(cx))),
            )
    }

    fn list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.busy.as_deref();
        let busy_action = self.busy_action;

        div()
            .id("service-list")
            .flex()
            .flex_col()
            .gap_4()
            .p(px(28.))
            .overflow_y_scroll()
            .size_full()
            .children(self.services.iter().map(|info| {
                let is_busy = busy == Some(info.name.as_str());
                self.service_card(info, is_busy, busy_action, cx)
            }))
    }

    /// A card rendered as an outer tray wrapping a raised inner surface.
    fn service_card(
        &self,
        info: &ServiceInfo,
        is_busy: bool,
        busy_action: Option<ServiceAction>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let can_act = !is_busy && !info.state.is_transitioning();

        let left = div()
            .flex()
            .flex_col()
            .flex_grow(1.0)
            .flex_shrink(1.0)
            .gap_2()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap_3()
                    .child(kind_badge(info.kind))
                    .child(
                        div()
                            .text_base()
                            .font_weight(FontWeight::MEDIUM)
                            .whitespace_normal()
                            .child(info.display_name.clone()),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("service \u{00b7} {}", info.name)),
            );

        let right = div()
            .flex()
            .flex_row()
            .flex_shrink(0.0)
            .items_center()
            .gap_4()
            .child(state_indicator(info.state, cx))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .children(action_buttons(
                        &info.name,
                        info.state,
                        is_busy,
                        busy_action,
                        can_act,
                        cx,
                    ))
                    .child(details_popover(info, self.ports.get(&info.name), cx)),
            );

        let core = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap_4()
            .p(px(18.))
            .rounded_lg()
            .bg(cx.theme().secondary)
            .child(left)
            .child(right);

        div()
            .p(px(5.))
            .rounded_xl()
            .bg(cx.theme().muted)
            .border_1()
            .border_color(cx.theme().border)
            .child(core)
    }
}

impl Render for Databaser {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        flush_toasts(&mut self.toasts, window, cx);

        let has_services = !self.services.is_empty();
        let body = if self.scanning {
            self.centered_message("Looking for database services\u{2026}", cx)
                .into_any_element()
        } else if has_services {
            self.list(cx).into_any_element()
        } else {
            self.centered_message("No local MySQL or PostgreSQL services were found.", cx)
                .into_any_element()
        };

        let mut root = div()
            .flex()
            .flex_col()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.header(cx));

        if let Some(message) = self.error.as_deref() {
            root = root.child(self.error_banner(message));
        }

        if let Some(banner) = self.update_banner(cx) {
            root = root.child(banner);
        }

        root.child(div().flex_1().overflow_hidden().child(body))
    }
}

fn theme_item(
    label: &str,
    pref: ThemePreference,
    current: ThemePreference,
    icon_name: IconName,
    entity: &Entity<Databaser>,
) -> PopupMenuItem {
    let entity = entity.clone();
    PopupMenuItem::new(label)
        .icon(icon(icon_name))
        .checked(current == pref)
        .on_click(move |_, window, cx| {
            entity.update(cx, |this, cx| this.set_theme(pref, window, cx));
        })
}

fn open_about(window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, |dialog, _window, cx| {
        dialog
            .title("About Databaser")
            .close_button(true)
            .w(px(440.))
            .child(about_content(cx))
    });
}

fn about_content(cx: &App) -> impl IntoElement {
    let muted = cx.theme().muted_foreground;

    div()
        .flex()
        .flex_col()
        .w_full()
        .gap_4()
        .py_2()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .w_full()
                .gap_4()
                .child(icon_tile(cx))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_lg()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("Databaser"),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(muted)
                                .child(format!("Version {}", env!("CARGO_PKG_VERSION"))),
                        ),
                ),
        )
        .child(
            div()
                .w_full()
                .text_sm()
                .text_color(muted)
                .child(env!("CARGO_PKG_DESCRIPTION")),
        )
        .child(div().h(px(1.)).w_full().bg(cx.theme().border))
        .child(
            div()
                .flex()
                .flex_col()
                .w_full()
                .gap_1()
                .child(div().text_xs().text_color(muted).child("SOURCE"))
                .child(
                    Link::new("about-repository")
                        .href(REPOSITORY_URL)
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .text_sm()
                        .child(icon(IconName::Github).small())
                        .child(repository_label()),
                ),
        )
        .child(
            div()
                .w_full()
                .text_xs()
                .text_color(muted)
                .child(format!(
                    "Built with Rust and GPUI Kit \u{00b7} {} licensed",
                    env!("CARGO_PKG_LICENSE")
                )),
        )
}

/// The repository URL without its scheme, as shown in the About dialog.
fn repository_label() -> &'static str {
    REPOSITORY_URL
        .strip_prefix("https://")
        .unwrap_or(REPOSITORY_URL)
}

/// The brand icon inside a nested tray, matching the card treatment.
fn icon_tile(cx: &App) -> impl IntoElement {
    div()
        .p(px(5.))
        .rounded_xl()
        .bg(cx.theme().muted)
        .border_1()
        .border_color(cx.theme().border)
        .child(
            div()
                .size(px(64.))
                .rounded_lg()
                .bg(cx.theme().secondary)
                .child(img(APP_ICON).size(px(64.))),
        )
}

fn action_buttons(
    name: &str,
    state: ServiceState,
    is_busy: bool,
    busy_action: Option<ServiceAction>,
    can_act: bool,
    cx: &mut Context<Databaser>,
) -> Vec<AnyElement> {
    if is_busy {
        let label = busy_action.map_or("Working", ServiceAction::ing_label);
        return vec![div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(Spinner::new().small())
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{label}\u{2026}")),
            )
            .into_any_element()];
    }

    if state.is_running() {
        let stop_name = name.to_string();
        let restart_name = name.to_string();
        vec![
            Button::new(format!("stop-{name}"))
                .icon(icon(IconName::Square))
                .tooltip("Stop")
                .danger()
                .disabled(!can_act)
                .on_click(cx.listener(move |this, _, _window, cx| {
                    this.request_action(stop_name.clone(), ServiceAction::Stop, cx)
                }))
                .into_any_element(),
            Button::new(format!("restart-{name}"))
                .icon(icon(IconName::RotateCcw))
                .tooltip("Restart")
                .outline()
                .disabled(!can_act)
                .on_click(cx.listener(move |this, _, _window, cx| {
                    this.request_action(restart_name.clone(), ServiceAction::Restart, cx)
                }))
                .into_any_element(),
        ]
    } else {
        let start_name = name.to_string();
        vec![Button::new(format!("start-{name}"))
            .icon(icon(IconName::Play))
            .tooltip("Start")
            .primary()
            .disabled(!can_act)
            .on_click(cx.listener(move |this, _, _window, cx| {
                this.request_action(start_name.clone(), ServiceAction::Start, cx)
            }))
            .into_any_element()]
    }
}

fn kind_badge(kind: ServiceKind) -> impl IntoElement {
    div()
        .px(px(7.))
        .py(px(2.))
        .rounded_full()
        .text_xs()
        .bg(kind_color(kind))
        .text_color(white())
        .child(kind.label().to_string())
}

/// A quiet, product-specific tint for the eyebrow badge.
fn kind_color(kind: ServiceKind) -> Rgba {
    match kind {
        ServiceKind::MySql => rgb(0x2f6f9f),
        ServiceKind::PostgreSql => rgb(0x41618a),
        ServiceKind::SqlServer => rgb(0x8f4147),
    }
}

/// Shorthand for an icon from the shared catalog.
/// A ghost "Details" button that opens a popover describing the service, and
/// letting its start type be changed.
///
/// The popover is content-sized: it clamps to the window rather than flipping, so
/// the trigger's anchor keeps it inside the card's column.
fn details_popover(
    info: &ServiceInfo,
    ports: Option<&Ports>,
    cx: &mut Context<Databaser>,
) -> impl IntoElement {
    let entity = cx.entity();
    let name = info.name.clone();
    let display_name = info.display_name.clone();
    let executable_path = info.executable_path.clone();
    let kind = info.kind;
    let state = info.state;
    let start_type = info.start_type;
    let version = info.version.clone();

    let ports_text = match ports {
        Some(Ports::Ready(ports)) if !ports.is_empty() => ports
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(", "),
        Some(Ports::Ready(_)) => "none".to_string(),
        Some(Ports::Unavailable) => "not running".to_string(),
        None | Some(Ports::Loading) => "\u{2026}".to_string(),
    };

    Popover::new(format!("details-{}", info.name))
        .anchor(Anchor::TopRight)
        .trigger(
            Button::new(format!("details-trigger-{}", info.name))
                .icon(icon(IconName::Info))
                .tooltip("Details")
                .ghost(),
        )
        .on_open_change({
            let entity = entity.clone();
            let service = name.clone();
            move |open, _window, cx| {
                if *open {
                    entity.update(cx, |this, cx| this.fetch_ports(service.clone(), cx));
                }
            }
        })
        .content(move |_state, _window, _cx| {
            let menu_entity = entity.clone();
            let menu_service = name.clone();
            let startup = Button::new(format!("startup-{}", name))
                .label(start_type.label())
                .outline()
                .small()
                .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                    menu.check_side(Side::Right)
                        .item(start_type_item(
                            "Automatic",
                            StartType::Automatic,
                            start_type,
                            &menu_service,
                            &menu_entity,
                        ))
                        .item(start_type_item(
                            "Manual",
                            StartType::Manual,
                            start_type,
                            &menu_service,
                            &menu_entity,
                        ))
                        .item(start_type_item(
                            "Disabled",
                            StartType::Disabled,
                            start_type,
                            &menu_service,
                            &menu_entity,
                        ))
                });

            // The default is a bordered 3-column grid. One column, unbordered,
            // gives a plain stack of `label  value` rows with the labels
            // aligned - which is what a details popover wants, and what a long
            // binary path needs in order to wrap instead of stretching the
            // popover off-screen.
            //
            // The version row is omitted entirely when it could not be read,
            // rather than showing an empty value.
            let mut list = DescriptionList::horizontal()
                .columns(1)
                .bordered(false)
                // `large` widens the gap between rows; the label column has to
                // fit the longest label ("Service name") or it runs into the
                // value beside it.
                .large()
                .label_width(px(104.))
                .child(DescriptionItem::new("Service").value(display_name.as_str()))
                .child(DescriptionItem::new("Service name").value(name.as_str()))
                .child(DescriptionItem::new("Type").value(kind.label()));
            if let Some(version) = version.as_deref() {
                list = list.child(DescriptionItem::new("Version").value(version));
            }
            list = list
                .child(DescriptionItem::new("State").value(state.label()))
                .child(
                    // A row wrapper stops the button stretching to fill the
                    // value column.
                    DescriptionItem::new("Start-up")
                        .value(div().flex().flex_row().child(startup).into_any_element()),
                )
                .child(DescriptionItem::new("Ports").value(ports_text.as_str()))
                .child(DescriptionItem::new("Binary").value(executable_path.as_str()));

            // A definite width keeps the popover a predictable size and makes
            // the values wrap instead of stretching it off-screen.
            div().w(px(400.)).child(list)
        })
}

/// One start type in the popover's start-up menu, ticked when it is current.
fn start_type_item(
    label: &str,
    start_type: StartType,
    current: StartType,
    service: &str,
    entity: &Entity<Databaser>,
) -> PopupMenuItem {
    let entity = entity.clone();
    let service = service.to_string();
    PopupMenuItem::new(label)
        .checked(current == start_type)
        .on_click(move |_, _, cx| {
            entity.update(cx, |this, cx| {
                this.request_action(
                    service.clone(),
                    ServiceAction::SetStartType(start_type),
                    cx,
                );
            });
        })
}

fn icon(name: IconName) -> Icon {
    Icon::new(name)
}

fn state_indicator(state: ServiceState, cx: &mut Context<Databaser>) -> impl IntoElement {
    let color = if state.is_running() {
        cx.theme().success
    } else if state.is_transitioning() {
        cx.theme().warning
    } else {
        cx.theme().muted_foreground
    };

    div()
        .flex()
        .flex_row()
        .flex_shrink(0.0)
        .items_center()
        .gap_2()
        .child(div().size(px(8.)).rounded_full().bg(color))
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(state.label()),
        )
}

fn flush_toasts(
    toasts: &mut Vec<(ToastKind, String)>,
    window: &mut Window,
    cx: &mut Context<Databaser>,
) {
    for (kind, message) in std::mem::take(toasts) {
        let notification = match kind {
            ToastKind::Success => Notification::success(message),
            ToastKind::Warning => Notification::warning(message),
            ToastKind::Error => Notification::error(message).autohide(false),
        };
        window.push_notification(notification, cx);
    }
}

/// Whether `candidate` is a later release than `current`.
///
/// Compares the dotted numeric parts. Anything unparsable - a prerelease tag,
/// say - counts as not-later, so a surprising tag can never raise a prompt.
/// Needed because the cached version is compared here rather than by the
/// updater, which is what normally does this against the running build.
fn is_newer(candidate: &str, current: &str) -> bool {
    fn parts(version: &str) -> Option<Vec<u64>> {
        version
            .trim_start_matches('v')
            .split('.')
            .map(|part| part.parse::<u64>().ok())
            .collect()
    }

    match (parts(candidate), parts(current)) {
        (Some(candidate), Some(current)) => candidate > current,
        _ => false,
    }
}

/// Whether an available version should be surfaced to the user.
///
/// A manual check always reports what it found, including a version the user
/// dismissed earlier; the automatic launch check stays quiet about it.
fn should_offer_update(manual: bool, available: &str, skipped: Option<&str>) -> bool {
    manual || skipped != Some(available)
}

/// Confirmation for a start-up type change.
///
/// A start type applies at boot: it neither stops a running service nor starts
/// a stopped one. Both look like nothing happened, so say which it is — that
/// is the difference between "it did not work" and "it will not take effect
/// until you reboot".
fn start_type_message(name: &str, start_type: StartType, running: bool) -> String {
    let mut message = format!("{name} \u{2014} {}", start_type.effect());
    match (running, start_type) {
        (true, StartType::Manual | StartType::Disabled) => {
            message.push_str(" It keeps running until you stop it.");
        }
        (false, StartType::Automatic) => message.push_str(" It will start at the next boot."),
        _ => {}
    }
    message
}

/// Closing the window hides it to the tray while "Run in background" is on, and
/// exits the app otherwise.
pub fn window_should_close(_window: &mut Window, cx: &mut App) -> bool {
    if tray::is_enabled(cx) {
        platform::show_window(platform::WINDOW_TITLE, false);
        return false;
    }
    true
}

/// Apply a three-way theme preference to the global theme.
pub fn apply_theme(pref: ThemePreference, window: Option<&mut Window>, cx: &mut App) {
    match pref {
        ThemePreference::Light => Theme::change(ThemeMode::Light, window, cx),
        ThemePreference::Dark => Theme::change(ThemeMode::Dark, window, cx),
        ThemePreference::System => Theme::sync_system_appearance(window, cx),
    }
}

#[cfg(test)]
mod tests {
    use super::{is_newer, should_offer_update, start_type_message, REPOSITORY_URL};
    use crate::services::StartType;
    use crate::update::{REPO_NAME, REPO_OWNER};

    #[test]
    fn a_cached_version_newer_than_ours_is_offered() {
        assert!(is_newer("0.3.0", "0.2.1"));
        assert!(is_newer("v0.2.2", "0.2.1"));
    }

    #[test]
    fn a_cached_version_we_already_run_is_not_offered() {
        assert!(!is_newer("0.2.1", "0.2.1"));
        assert!(!is_newer("0.1.9", "0.2.1"));
    }

    #[test]
    fn an_unparsable_cached_version_is_not_offered() {
        assert!(!is_newer("0.3.0-rc1", "0.2.1"));
        assert!(!is_newer("nightly", "0.2.1"));
    }

    #[test]
    fn disabling_a_running_service_says_it_keeps_running() {
        let message = start_type_message("MSSQL$SQLEXPRESS", StartType::Disabled, true);
        assert!(message.contains("won't start until it is re-enabled"), "{message}");
        assert!(message.contains("keeps running until you stop it"), "{message}");
    }

    #[test]
    fn disabling_a_stopped_service_says_nothing_extra() {
        let message = start_type_message("MSSQL$SQLEXPRESS", StartType::Disabled, false);
        assert!(!message.contains("keeps running"), "{message}");
    }

    #[test]
    fn setting_automatic_on_a_stopped_service_says_it_waits_for_boot() {
        let message = start_type_message("MySQL80", StartType::Automatic, false);
        assert!(message.contains("at the next boot"), "{message}");
    }

    #[test]
    fn setting_automatic_on_a_running_service_says_nothing_extra() {
        let message = start_type_message("MySQL80", StartType::Automatic, true);
        assert!(!message.contains("next boot"), "{message}");
    }

    #[test]
    fn about_dialog_repository_url_matches_the_updater() {
        assert_eq!(
            REPOSITORY_URL,
            format!("https://github.com/{REPO_OWNER}/{REPO_NAME}"),
            "Cargo.toml's repository and the updater's release target must agree"
        );
    }

    #[test]
    fn manual_check_offers_any_available_version() {
        assert!(should_offer_update(true, "0.2.0", None));
    }

    #[test]
    fn automatic_check_offers_a_newer_version() {
        assert!(should_offer_update(false, "0.2.0", Some("0.1.9")));
    }

    #[test]
    fn automatic_check_stays_quiet_for_a_skipped_version() {
        assert!(!should_offer_update(false, "0.2.0", Some("0.2.0")));
    }

    #[test]
    fn manual_check_still_offers_a_skipped_version() {
        assert!(should_offer_update(true, "0.2.0", Some("0.2.0")));
    }
}

