//! Local models served by Ollama: the settings, the installed-model list (fetched off the UI
//! thread) and the Settings card. OpenCode gets them as an extra provider at spawn time.

use gpui_kit::component::Sizable as _;
use gpui_kit::component::input::Input;
use gpui_kit::component::select::Select;
use hangar_agents::ollama::{self, OllamaHost, OllamaModel};
use serde_json::{Map, Value};

use super::*;
use crate::ui::components::{Segmented, UiButton, card};
use crate::ui::{ActiveTokens as _, TypeScale as _};

const LIST_TIMEOUT: Duration = Duration::from_secs(2);

pub(super) type ModelSelect = SelectState<Vec<AgentOption>>;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) enum OllamaStatus {
    #[default]
    Unknown,
    Loading,
    Ready(Vec<OllamaModel>),
    Failed(String),
}

/// What the app keeps about Ollama.
pub(super) struct OllamaState {
    pub settings: ops::OllamaSettings,
    pub status: OllamaStatus,
    pub host_input: Entity<InputState>,
    pub model_select: Entity<ModelSelect>,
    /// The running model-list fetch; replacing it drops the stale one.
    refresh: Option<Task<()>>,
}

impl OllamaState {
    pub fn new(window: &mut Window, cx: &mut Context<HangarApp>) -> (Self, Vec<Subscription>) {
        let host_input = cx.new(|cx| InputState::new(window, cx).placeholder(ollama::DEFAULT_HOST));
        let model_select = cx.new(|cx| SelectState::new(Vec::new(), None, window, cx));
        let subscriptions = vec![
            cx.subscribe_in(
                &host_input,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                        this.commit_ollama_host(window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &model_select,
                window,
                |this, _, event: &SelectEvent<Vec<AgentOption>>, window, cx| {
                    let SelectEvent::Confirm(model) = event;
                    this.ollama.settings.default_model = model.as_ref().map(|m| m.to_string());
                    this.save_ollama(window, cx);
                    cx.notify();
                },
            ),
        ];
        let state = Self {
            settings: ops::OllamaSettings::default(),
            status: OllamaStatus::Unknown,
            host_input,
            model_select,
            refresh: None,
        };
        (state, subscriptions)
    }

    /// The OpenCode provider config for the last fetched model list, if Ollama is enabled and
    /// has models.
    pub fn opencode_provider(&self) -> Option<Map<String, Value>> {
        if !self.settings.enabled {
            return None;
        }
        let OllamaStatus::Ready(models) = &self.status else {
            return None;
        };
        let host = OllamaHost::parse(&self.settings.host).ok()?;
        (!models.is_empty()).then(|| {
            ollama::opencode_provider(&host, models, self.settings.default_model.as_deref())
        })
    }
}

impl HangarApp {
    /// Applies the saved settings once the store is loaded.
    pub(super) fn restore_ollama(
        &mut self,
        settings: ops::OllamaSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let host = settings.host.clone();
        self.ollama
            .host_input
            .update(cx, |input, cx| input.set_value(host, window, cx));
        self.ollama.settings = settings;
        if self.ollama.settings.enabled {
            self.refresh_ollama(window, cx);
        }
    }

    pub(super) fn set_ollama_enabled(
        &mut self,
        enabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ollama.settings.enabled = enabled;
        self.save_ollama(window, cx);
        if enabled {
            self.refresh_ollama(window, cx);
        }
        cx.notify();
    }

    /// `HANGAR_DEV_OLLAMA`: turns Ollama on at `host` for this run without saving it. Resolves
    /// once the model list is in.
    pub(super) fn use_ollama_for_run(
        &mut self,
        host: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        tracing::info!(%host, "HANGAR_DEV_OLLAMA");
        self.ollama.settings.enabled = true;
        self.ollama.settings.host = host.clone();
        self.ollama
            .host_input
            .update(cx, |input, cx| input.set_value(host, window, cx));
        self.ollama.refresh = None;
        self.fetch_ollama_models(window, cx)
    }

    fn commit_ollama_host(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let host = self.ollama.host_input.read(cx).value().trim().to_owned();
        if host == self.ollama.settings.host {
            return;
        }
        self.ollama.settings.host = host;
        self.save_ollama(window, cx);
        self.refresh_ollama(window, cx);
    }

    fn save_ollama(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let settings = self.ollama.settings.clone();
        self.persist(window, cx, move |store| ops::save_ollama(store, &settings));
    }

    /// Fetches the installed models on the background executor.
    pub(super) fn refresh_ollama(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ollama.refresh = Some(self.fetch_ollama_models(window, cx));
    }

    fn fetch_ollama_models(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Task<()> {
        let host = match OllamaHost::parse(&self.ollama.settings.host) {
            Ok(host) => host,
            Err(err) => {
                self.set_ollama_status(OllamaStatus::Failed(err.to_string()), window, cx);
                return Task::ready(());
            }
        };
        self.ollama.status = OllamaStatus::Loading;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let listed = cx
                .background_executor()
                .spawn(async move { ollama::list_models(&host, LIST_TIMEOUT) })
                .await;
            let status = match listed {
                Ok(models) => {
                    let names: Vec<&str> = models.iter().map(|m| m.name.as_str()).collect();
                    tracing::info!(?names, "Ollama models");
                    OllamaStatus::Ready(models)
                }
                Err(err) => {
                    tracing::warn!("cannot list Ollama models: {err}");
                    OllamaStatus::Failed(err.to_string())
                }
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.set_ollama_status(status, window, cx)
            });
        })
    }

    fn set_ollama_status(
        &mut self,
        status: OllamaStatus,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let OllamaStatus::Ready(models) = &status {
            let options: Vec<AgentOption> = models
                .iter()
                .map(|m| AgentOption {
                    id: m.name.clone().into(),
                    title: match &m.parameter_size {
                        Some(size) => format!("{} · {size}", m.name).into(),
                        None => m.name.clone().into(),
                    },
                })
                .collect();
            let selected: Option<SharedString> = self
                .ollama
                .settings
                .default_model
                .clone()
                .filter(|m| models.iter().any(|known| known.name == *m))
                .map(Into::into);
            self.ollama.model_select.update(cx, |select, cx| {
                select.set_items(options, window, cx);
                if let Some(model) = &selected {
                    select.set_selected_value(model, window, cx);
                }
            });
        }
        self.ollama.status = status;
        cx.notify();
    }

    /// The warning to show when `agent_id` should run on Ollama but has no models to use.
    pub(super) fn ollama_unavailable(&self, agent_id: &str) -> Option<String> {
        if !self.ollama.settings.enabled || !agents::supports_ollama(agent_id) {
            return None;
        }
        let host = OllamaHost::parse(&self.ollama.settings.host)
            .map(|h| h.url())
            .unwrap_or_else(|_| self.ollama.settings.host.clone());
        match &self.ollama.status {
            OllamaStatus::Ready(models) if !models.is_empty() => None,
            OllamaStatus::Ready(_) => Some(format!(
                "Ollama at {host} has no models (ollama pull <model>); the agent starts without them."
            )),
            OllamaStatus::Failed(err) => Some(format!(
                "Ollama is not available ({err}); the agent starts without local models."
            )),
            OllamaStatus::Unknown | OllamaStatus::Loading => Some(format!(
                "Ollama models at {host} are still loading; the agent starts without them."
            )),
        }
    }

    pub(super) fn render_ollama_card(&self, cx: &mut Context<Self>) -> Div {
        let t = cx.tokens();
        let enabled = self.ollama.settings.enabled;
        let (status, status_color) = match &self.ollama.status {
            _ if !enabled => ("Off".to_owned(), t.tx3),
            OllamaStatus::Unknown => ("Not checked".to_owned(), t.tx3),
            OllamaStatus::Loading => ("Checking…".to_owned(), t.tx3),
            OllamaStatus::Ready(models) if models.is_empty() => (
                "Running · no models (ollama pull <model>)".to_owned(),
                t.tx2,
            ),
            OllamaStatus::Ready(models) => (
                format!(
                    "Running · {} model{}",
                    models.len(),
                    if models.len() == 1 { "" } else { "s" }
                ),
                t.grn,
            ),
            OllamaStatus::Failed(err) => (err.clone(), t.red),
        };
        let row = || {
            h_flex()
                .px(px(14.))
                .py_3()
                .gap_3()
                .items_center()
                .justify_between()
                .border_b_1()
                .border_color(t.line)
        };
        let label = |title: &'static str, hint: &'static str| {
            v_flex()
                .gap_0p5()
                .min_w_0()
                .child(div().type_body_strong().child(title))
                .child(div().type_secondary().text_color(t.tx3).child(hint))
        };
        card(cx)
            .child(
                row()
                    .child(label(
                        "Ollama (local models)",
                        "Offers your local models to OpenCode. Pick models with tool calling \
                         (e.g. qwen3-coder) and raise OLLAMA_CONTEXT_LENGTH for agent work.",
                    ))
                    .child(
                        Segmented::new("ollama-enabled", ["Off", "On"])
                            .selected(usize::from(enabled))
                            .on_select(cx.listener(|this, i: &usize, window, cx| {
                                this.set_ollama_enabled(*i == 1, window, cx)
                            })),
                    ),
            )
            .when(enabled, |this| {
                this.child(
                    row()
                        .child(label("Host", "Plain HTTP; press Enter to apply."))
                        .child(
                            div()
                                .w(px(260.))
                                .child(Input::new(&self.ollama.host_input).small()),
                        ),
                )
                .child(
                    row()
                        .child(label(
                            "Default model",
                            "OpenCode starts on it; the others stay in its model menu.",
                        ))
                        .child(
                            div().w(px(260.)).child(
                                Select::new(&self.ollama.model_select)
                                    .small()
                                    .placeholder("OpenCode's own default"),
                            ),
                        ),
                )
                .child(
                    row()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .font_mono()
                                .text_size(px(11.))
                                .text_color(status_color)
                                .truncate()
                                .child(status),
                        )
                        .child(
                            UiButton::new("ollama-refresh")
                                .label("Refresh")
                                .small()
                                .disabled(self.ollama.status == OllamaStatus::Loading)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.refresh_ollama(window, cx)
                                })),
                        ),
                )
            })
    }
}
