//! The Databaser window: header with the theme switch, a list of discovered
//! database service cards, and inline/transient feedback.

use std::time::Duration;

use gpui_kit::base::Disableable as _;
use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, Theme, ThemeMode, WindowExt as _};
use gpui_kit::*;

use crate::elevation::{self, ActionOutcome, ControlMode};
use crate::helper_task::{self, TaskError};
use crate::services::{self, ServiceAction, ServiceInfo, ServiceKind, ServiceState};
use crate::settings::{Settings, ThemePreference};

/// How often the service states are re-read while the window is open.
const POLL_INTERVAL: Duration = Duration::from_millis(2000);

#[derive(Clone, Copy)]
enum ToastKind {
    Success,
    Warning,
    Error,
}

pub struct Databaser {
    services: Vec<ServiceInfo>,
    /// Name of the service currently being acted on, if any.
    busy: Option<String>,
    busy_action: Option<ServiceAction>,
    scanning: bool,
    /// True while the no-prompt scheduled task is being installed.
    setting_up: bool,
    /// How privileged actions are performed (probed on start-up).
    mode: ControlMode,
    /// A persistent problem (e.g. discovery failed), shown as a banner.
    error: Option<String>,
    theme: ThemePreference,
    settings: Settings,
    toasts: Vec<(ToastKind, String)>,
}

impl Databaser {
    pub fn new(settings: Settings, cx: &mut Context<Self>) -> Self {
        let this = Self {
            services: Vec::new(),
            busy: None,
            busy_action: None,
            scanning: true,
            setting_up: false,
            mode: ControlMode::Uac,
            error: None,
            theme: settings.theme,
            settings,
            toasts: Vec::new(),
        };

        // Discovery, the elevation-mode probe, and state polling all touch
        // blocking Windows APIs, so they run on the background executor and
        // only publish results to the UI.
        Self::spawn_discovery(cx);
        Self::spawn_mode_probe(cx);
        Self::spawn_poll(cx);

        this
    }

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

    /// Re-discover services (used by the Rescan button).
    fn rescan(&mut self, cx: &mut Context<Self>) {
        if self.scanning {
            return;
        }
        self.scanning = true;
        cx.notify();
        Self::spawn_discovery(cx);
    }

    /// Re-read service states on a background thread so the UI thread never
    /// blocks on the SCM.
    fn spawn_poll(cx: &mut Context<Self>) {
        cx.spawn(async move |entity, cx| {
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;

                // Collect the names to check on the UI thread (cheap), then do
                // the actual SCM queries off it.
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

    /// Install the no-prompt scheduled task (one UAC prompt).
    fn setup_elevated(&mut self, cx: &mut Context<Self>) {
        if self.setting_up {
            return;
        }
        self.setting_up = true;
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
                    this.setting_up = false;
                    match result {
                        Ok(()) => {
                            this.mode = ControlMode::Task;
                            this.push_toast(
                                ToastKind::Success,
                                "Elevated control is ready \u{2014} start/stop no longer prompts."
                                    .to_string(),
                            );
                        }
                        Err(TaskError::Cancelled) => this.push_toast(
                            ToastKind::Warning,
                            "Setup cancelled \u{2014} administrator rights are required once."
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

    /// Start / stop / restart, using whichever control mode is available.
    ///
    /// The work can block on a UAC prompt or on the helper process, so it runs
    /// on the background executor rather than the UI thread.
    fn request_action(&mut self, name: String, action: ServiceAction, cx: &mut Context<Self>) {
        if self.busy.is_some() {
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
                        ActionOutcome::Done => this.push_toast(
                            ToastKind::Success,
                            format!("{} {}", action.done_label(), name),
                        ),
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

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let scanning = self.scanning;

        let mut actions = div().flex().flex_row().items_center().gap_2();

        if self.mode == ControlMode::Uac {
            let setting_up = self.setting_up;
            actions = actions.child(
                Button::new("setup-elevated")
                    .small()
                    .label(if setting_up {
                        "Setting up\u{2026}"
                    } else {
                        "Enable no-prompt control"
                    })
                    .disabled(setting_up)
                    .on_click(cx.listener(|this, _, _window, cx| this.setup_elevated(cx))),
            );
        }

        actions = actions
            .child(
                Button::new("rescan")
                    .ghost()
                    .small()
                    .label(if scanning { "Scanning\u{2026}" } else { "Rescan" })
                    .on_click(cx.listener(|this, _, _window, cx| this.rescan(cx))),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .child(self.theme_button("System", ThemePreference::System, theme, cx))
                    .child(self.theme_button("Light", ThemePreference::Light, theme, cx))
                    .child(self.theme_button("Dark", ThemePreference::Dark, theme, cx)),
            );

        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Databaser"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "v{} \u{00b7} {}",
                                env!("CARGO_PKG_VERSION"),
                                self.mode.label()
                            )),
                    ),
            )
            .child(actions)
    }

    fn theme_button(
        &self,
        label: &'static str,
        pref: ThemePreference,
        current: ThemePreference,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let base = Button::new(label).small().label(label);
        let styled = if current == pref {
            base.primary()
        } else {
            base.ghost()
        };
        styled.on_click(cx.listener(move |this, _, window, cx| this.set_theme(pref, window, cx)))
    }

    fn error_banner(&self, message: &str) -> impl IntoElement {
        div().px_4().pt_3().child(
            Alert::error("discovery-error", message.to_string())
                .title("Something went wrong")
                .banner(),
        )
    }

    fn centered_message(&self, text: &str, cx: &mut Context<Self>) -> impl IntoElement {
        let scanning = self.scanning;
        div()
            .flex()
            .flex_col()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .child(if scanning {
                Spinner::new().into_any_element()
            } else {
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(text.to_string())
                    .into_any_element()
            })
            .child(
                Button::new("rescan-empty")
                    .label("Scan again")
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
            .gap_3()
            .p_4()
            .overflow_y_scroll()
            .size_full()
            .children(self.services.iter().map(|info| {
                let is_busy = busy == Some(info.name.as_str());
                self.service_card(info, is_busy, busy_action, cx)
            }))
    }

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
            .gap_1()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(kind_badge(info.kind, cx))
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .child(info.display_name.clone()),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("service: {}", info.name)),
            );

        let right = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
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
                    )),
            );

        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap_3()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().secondary)
            .child(left)
            .child(right)
    }
}

impl ControlMode {
    fn label(self) -> &'static str {
        match self {
            Self::Elevated => "elevated",
            Self::Task => "no-prompt control",
            Self::Uac => "prompt per action",
        }
    }
}

impl Render for Databaser {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        flush_toasts(&mut self.toasts, window, cx);

        let scanning = self.scanning;
        let has_services = !self.services.is_empty();

        let body = if scanning {
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

        root.child(div().flex_1().overflow_hidden().child(body))
    }
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
                .label("Stop")
                .danger()
                .disabled(!can_act)
                .on_click(cx.listener(move |this, _, _window, cx| {
                    this.request_action(stop_name.clone(), ServiceAction::Stop, cx)
                }))
                .into_any_element(),
            Button::new(format!("restart-{name}"))
                .label("Restart")
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
            .label("Start")
            .primary()
            .disabled(!can_act)
            .on_click(cx.listener(move |this, _, _window, cx| {
                this.request_action(start_name.clone(), ServiceAction::Start, cx)
            }))
            .into_any_element()]
    }
}

fn kind_badge(kind: ServiceKind, cx: &mut Context<Databaser>) -> impl IntoElement {
    div()
        .px(px(8.))
        .py(px(2.))
        .rounded_md()
        .text_xs()
        .bg(cx.theme().accent)
        .text_color(cx.theme().accent_foreground)
        .child(kind.label())
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

/// Apply a three-way theme preference to the global theme.
pub fn apply_theme(pref: ThemePreference, window: Option<&mut Window>, cx: &mut App) {
    match pref {
        ThemePreference::Light => Theme::change(ThemeMode::Light, window, cx),
        ThemePreference::Dark => Theme::change(ThemeMode::Dark, window, cx),
        ThemePreference::System => Theme::sync_system_appearance(window, cx),
    }
}
