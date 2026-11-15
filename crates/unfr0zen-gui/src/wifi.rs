//! The Patch page's Wi-Fi side: the address of a unit patched with nestlocal
//! updates over Wi-Fi, Force, and the code its screen shows while a run
//! waits on it.

use std::sync::mpsc::Sender;

use eframe::egui;
use unfr0zen_host::push;

use crate::{backdrop, tr};

/// The Wi-Fi inputs, and the code the running Wi-Fi run waits on.
#[derive(Default)]
pub struct Wifi {
    pub address: String,
    /// Send this app's nestlocal even when it is older than the unit's.
    pub force: bool,
    ask: Option<Ask>,
}

/// The run's question: its prompt, the code typed so far, and where the
/// answer goes; none is Cancel.
struct Ask {
    prompt: String,
    code: String,
    reply: Sender<Option<String>>,
    focused: bool,
}

impl Wifi {
    /// The address given, once it is one.
    pub fn host(&self) -> Result<String, String> {
        let host = self.address.trim();
        push::address(host).map_err(|e| e.to_string())?;
        Ok(host.to_string())
    }

    pub fn ask(&mut self, prompt: String, reply: Sender<Option<String>>) {
        self.ask = Some(Ask { prompt, code: String::new(), reply, focused: false });
    }

    pub fn asking(&self) -> bool {
        self.ask.is_some()
    }

    /// Answers the run and closes the question; whether it was cancelled.
    fn answer(&mut self, code: Option<String>) -> bool {
        let cancelled = code.is_none();
        if let Some(ask) = self.ask.take() {
            let _ = ask.reply.send(code);
        }
        cancelled
    }

    /// The unit's address, beside Connect on the Connect step.
    pub fn address_ui(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new(tr!("push.address")).color(crate::MUTED));
        crate::setup::small_boxed(ui, egui::TextEdit::singleline(&mut self.address).hint_text("192.168.1.50"), 200.0)
            .on_hover_text(tr!("push.address_hint"));
    }

    /// The code dialog while the run waits on it. Returns whether it was
    /// cancelled, once answered: Cancel, Escape and the backdrop answer none.
    pub fn dialog(&mut self, ctx: &egui::Context) -> Option<bool> {
        let ask = self.ask.as_mut()?;
        let response = backdrop::modal(ctx, "push_code").show(ctx, |ui| {
            ui.set_max_width(320.0);
            crate::dialog_title(ui, tr!("push.code_title"));
            ui.add(egui::Label::new(ask.prompt.as_str()).wrap());
            let field = ui.add(egui::TextEdit::singleline(&mut ask.code).char_limit(8).hint_text(tr!("push.code_hint"))
                .font(egui::FontId::monospace(16.0)).desired_width(120.0));
            ask.code.retain(|c| c.is_ascii_digit());
            if !ask.focused {
                field.request_focus();
                ask.focused = true;
            }
            let valid = push::is_code(&ask.code);
            let entered = valid && field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let mut answer = None;
            ui.horizontal(|ui| {
                if crate::setup::page_button(ui, tr!("common.cancel"), crate::setup::Look::Secondary, true).clicked() {
                    answer = Some(None);
                }
                if crate::setup::page_button(ui, tr!("push.send"), crate::setup::Look::Primary, valid).clicked() || entered {
                    answer = Some(Some(ask.code.clone()));
                }
            });
            answer
        });
        match response.inner {
            Some(code) => Some(self.answer(code)),
            None if response.should_close() => Some(self.answer(None)),
            None => None,
        }
    }
}
