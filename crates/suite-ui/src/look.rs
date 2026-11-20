//! Light or dark: both windows' `Theme.mode` follow one choice, which can change while
//! they're open.

use std::cell::{Cell, RefCell};

use serde::{Deserialize, Serialize};
use slint::ComponentHandle;
use suite_ui_markup::ui;

/// What Settings → Appearance offers, in its order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeChoice {
    /// Follows the OS, dark when it doesn't say.
    #[default]
    System,
    Light,
    Dark,
}

impl ThemeChoice {
    pub(crate) const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];

    pub(crate) fn index(self) -> i32 {
        match self {
            Self::System => 0,
            Self::Light => 1,
            Self::Dark => 2,
        }
    }
}

type Apply = Box<dyn Fn(ThemeChoice) -> bool>;

thread_local! {
    static CHOICE: Cell<ThemeChoice> = const { Cell::new(ThemeChoice::System) };
    static WINDOWS: RefCell<Vec<Apply>> = const { RefCell::new(Vec::new()) };
}

/// A window whose markup has the `Theme` global.
pub(crate) trait Themed: ComponentHandle + 'static {
    fn set_mode(&self, choice: ThemeChoice);
}

/// Applies the current choice to `window` now and on every later change, until it's dropped.
pub(crate) fn follow<W: Themed>(window: &W) {
    window.set_mode(CHOICE.get());
    let weak = window.as_weak();
    WINDOWS.with_borrow_mut(|w| {
        w.push(Box::new(move |choice| weak.upgrade().map(|w| w.set_mode(choice)).is_some()));
    });
}

pub(crate) fn set(choice: ThemeChoice) {
    CHOICE.set(choice);
    WINDOWS.with_borrow_mut(|w| w.retain(|apply| apply(choice)));
}

pub(crate) fn current() -> ThemeChoice {
    CHOICE.get()
}

macro_rules! themed {
    ($($window:ident),+) => {$(
        impl Themed for ui::$window {
            fn set_mode(&self, choice: ThemeChoice) {
                self.global::<ui::Theme>().set_mode(match choice {
                    ThemeChoice::System => ui::ThemeMode::System,
                    ThemeChoice::Dark => ui::ThemeMode::Dark,
                    ThemeChoice::Light => ui::ThemeMode::Light,
                });
            }
        }
    )+};
}

themed!(AppWindow, InstallWindow);
