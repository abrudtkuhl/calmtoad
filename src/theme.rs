//! The app's look: a near-black specification sheet.
//!
//! Hairlines instead of cards, mono for measurements and micro-labels, sans for the page
//! content itself, and one white action. Dark and light are the same system derived from one
//! seed, so the window keeps following macOS.

use rune::color::hex;
use rune::{Color, Shadow, TextStyle};
use rune_kit::{Kit, PaletteFocus, PaletteSeed, StateColors, Type};

fn ink(value: &str) -> Color {
    hex(value).expect("theme colours are valid hex")
}

/// A status role keeps its derived wash, with the signal colour tuned for the ground it sits on.
fn role(derived: StateColors, fg: Color) -> StateColors {
    StateColors {
        fg,
        icon: fg,
        ..derived
    }
}

fn mono(size: f32, weight: f32, tracking: f32) -> TextStyle {
    TextStyle::new(size, weight)
        .mono()
        .lh(size + 4.0)
        .tracking(tracking)
}

pub fn app(dark: bool) -> Kit {
    let (bg, fg) = if dark {
        ("#0a0a0a", "#ededed")
    } else {
        ("#fafafa", "#111111")
    };
    let mut k = Kit::from_palette_focus(
        PaletteSeed {
            bg: ink(bg),
            fg: ink(fg),
            accent: ink(fg),
        },
        PaletteFocus::Ink,
    );

    // Ground and surfaces. Fields sit a step above the page; nothing is a raised card.
    k.c.page = ink(bg);
    k.c.bg_deep = k.c.page;
    k.c.bg_base = if dark { ink("#0d0d0d") } else { Color::WHITE };
    k.c.layer_01 = if dark { ink("#121212") } else { ink("#f2f2f2") };
    k.c.bg_button_neutral = if dark { ink("#161616") } else { ink("#ececec") };
    k.c.border_weak = if dark { ink("#1f1f1f") } else { ink("#e4e4e4") };
    k.c.border_muted = if dark { ink("#262626") } else { ink("#dcdcdc") };
    k.c.border_base = if dark { ink("#2a2a2a") } else { ink("#d4d4d4") };
    k.c.border_strong = if dark { ink("#3a3a3a") } else { ink("#b8b8b8") };
    // Faint is a placeholder, not a whisper: keep it at reading contrast.
    k.c.text_faint = if dark { ink("#8a8a8a") } else { ink("#6b6b6b") };

    // Functional colour only where the crawl needs it, and quiet enough for a ledger.
    k.c.success = role(k.c.success, ink(if dark { "#3fb950" } else { "#1a7f37" }));
    k.c.warning = role(k.c.warning, ink(if dark { "#d29922" } else { "#9a6700" }));
    k.c.danger = role(k.c.danger, ink(if dark { "#f85149" } else { "#cf222e" }));

    // A control's outline is a hairline ring, not a shadow; the primary action is flat.
    k.elevation.button_neutral = vec![Shadow {
        spread: 1.0,
        color: k.c.border_base,
        ..Shadow::default()
    }];
    k.elevation.button_contrast.clear();
    k.elevation.raised.clear();
    k.elevation.elements.clear();

    k.radius = rune_kit::Radius {
        xs: 1.0,
        sm: 2.0,
        md: 2.0,
        lg: 4.0,
        xl: 4.0,
    };

    let ui = TextStyle::new(12.5, 400.0).lh(18.0);
    let micro = mono(10.0, 500.0, 0.6);
    k.ty = Type {
        ui,
        ui_medium: TextStyle::new(12.5, 500.0).lh(18.0),
        ui_compact: TextStyle::new(12.5, 400.0).lh(18.0),
        small: micro,
        small_medium: micro,
        body: TextStyle::new(13.0, 400.0).lh(19.0),
        body_medium: TextStyle::new(13.0, 500.0).lh(19.0),
        text_small: TextStyle::new(12.5, 400.0).lh(18.0),
        text_small_medium: TextStyle::new(12.5, 500.0).lh(18.0),
        text_xsmall: TextStyle::new(11.5, 400.0).lh(17.0),
        text_xsmall_medium: TextStyle::new(11.5, 500.0).lh(17.0),
        mono: mono(11.5, 400.0, 0.0).features("'tnum' 1"),
    };

    k
}
