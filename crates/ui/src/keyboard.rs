//! The editor's (and the viewer window's) keys for `chan.keyboard_in`: passed on to the
//! engine's keyboard hub while such a node exists. Keys typed into a text field and shortcuts
//! (with Ctrl, Alt or Cmd) are left out; releases always go through, so nothing sticks.

use egui::{Context, Event};
use penumbra_ops::keyboard::{Key, Keyboard, Source};

/// The key an egui key means: by its name, else by the physical key under it (Shift+1 is
/// still 1).
pub fn key_of(key: egui::Key, physical: Option<egui::Key>) -> Option<Key> {
    Key::parse(key.name()).or_else(|| physical.and_then(|p| Key::parse(p.name())))
}

/// Passes this pass's key events of `ctx`'s viewport on to `keyboard`, as `source`.
pub fn feed(ctx: &Context, keyboard: &Keyboard, source: Source) {
    if !keyboard.wanted() {
        return;
    }
    let typing = ctx.text_edit_focused();
    ctx.input(|i| {
        for e in &i.events {
            match e {
                Event::Key { key, physical_key, pressed, repeat, modifiers } => {
                    if *repeat {
                        continue;
                    }
                    let shortcut = modifiers.ctrl || modifiers.alt || modifiers.command || modifiers.mac_cmd;
                    let meant =
                        if *pressed && !typing && !shortcut { key_of(*key, *physical_key) } else { None };
                    keyboard.key(source, physical_key.unwrap_or(*key).name(), meant, *pressed);
                }
                Event::WindowFocused(false) => keyboard.release_all(source),
                _ => {}
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use egui::{Modifiers, RawInput};

    use super::*;

    fn key_event(key: egui::Key, pressed: bool, modifiers: Modifiers) -> Event {
        Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers }
    }

    fn pass(ctx: &Context, kb: &Keyboard, events: Vec<Event>, text_field: bool) {
        let input = RawInput { events, ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| {
            feed(ui.ctx(), kb, Source::Editor);
            let mut text = String::new();
            let r = ui.text_edit_singleline(&mut text);
            if text_field {
                r.request_focus();
            } else {
                r.surrender_focus();
            }
        });
        out.textures_delta.clear();
    }

    #[test]
    fn keys_reach_the_hub_but_not_from_a_text_field_or_with_ctrl() {
        let ctx = Context::default();
        let kb = Keyboard::new();
        let one = [Key::parse("1").unwrap()];
        kb.set_wanted(true);
        pass(&ctx, &kb, vec![], false);
        pass(&ctx, &kb, vec![key_event(egui::Key::Num1, true, Modifiers::NONE)], false);
        assert!(kb.view(&one)[0].held);
        pass(&ctx, &kb, vec![key_event(egui::Key::Num1, false, Modifiers::NONE)], false);
        assert!(!kb.view(&one)[0].held);
        assert_eq!(kb.view(&one)[0].presses, 1);
        // Ctrl+1 is a shortcut.
        pass(&ctx, &kb, vec![key_event(egui::Key::Num1, true, Modifiers::CTRL)], false);
        pass(&ctx, &kb, vec![key_event(egui::Key::Num1, false, Modifiers::CTRL)], false);
        assert_eq!(kb.view(&one)[0].presses, 1);
        // Typed into a text field (focused since the pass before).
        pass(&ctx, &kb, vec![], true);
        pass(&ctx, &kb, vec![key_event(egui::Key::Num1, true, Modifiers::NONE)], true);
        pass(&ctx, &kb, vec![key_event(egui::Key::Num1, false, Modifiers::NONE)], true);
        assert_eq!(kb.view(&one)[0].presses, 1, "a key typed into a text field does not fire");
        // Held when the window loses the focus: let go.
        pass(&ctx, &kb, vec![], false);
        pass(&ctx, &kb, vec![], false);
        pass(&ctx, &kb, vec![key_event(egui::Key::Num1, true, Modifiers::NONE)], false);
        assert!(kb.view(&one)[0].held);
        pass(&ctx, &kb, vec![Event::WindowFocused(false)], false);
        assert!(!kb.view(&one)[0].held);
        // Without a keyboard node nothing is passed on.
        kb.set_wanted(false);
        pass(&ctx, &kb, vec![key_event(egui::Key::Num1, true, Modifiers::NONE)], false);
        assert_eq!(kb.view(&one)[0].presses, 2);
    }

    #[test]
    fn shift_and_the_number_pad_still_mean_the_digit() {
        assert_eq!(key_of(egui::Key::Exclamationmark, Some(egui::Key::Num1)).map(Key::name), Some("1"));
        assert_eq!(
            key_of(egui::Key::A, Some(egui::Key::Q)).map(Key::name),
            Some("a"),
            "as the layout labels it"
        );
        assert_eq!(key_of(egui::Key::ArrowLeft, None).map(Key::name), Some("left"));
        assert_eq!(key_of(egui::Key::F5, None).map(Key::name), Some("f5"));
        assert_eq!(key_of(egui::Key::Space, None).map(Key::name), Some("space"));
        assert_eq!(key_of(egui::Key::Comma, None), None);
    }
}
