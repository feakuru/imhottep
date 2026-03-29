use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{CurrentScreen, EditingField, FocusableField};

// ── Context ───────────────────────────────────────────────────────────────────

/// Identifies the complete app context for keybinding lookup.
///
/// A `None` `editing` field means navigation mode; a `None` `focus` field is
/// only used in editing mode (where focus is irrelevant for most bindings).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyContext {
    pub screen: CurrentScreen,
    /// `Some(field)` means editing mode; `None` means navigation mode.
    pub editing: Option<EditingField>,
    /// The currently focused field (relevant in navigation mode).
    pub focus: FocusableField,
}

// ── Action ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    // ── Global ────────────────────────────────────────────────────────────────
    /// Show the exit confirmation popup (from any navigation context).
    TriggerExit,

    // ── Main screen ───────────────────────────────────────────────────────────
    NewRequest,
    DeleteRequest,
    SelectNextRequest,
    SelectPreviousRequest,
    EditRequest,
    SaveRequests,

    // ── Exit confirmation ─────────────────────────────────────────────────────
    ConfirmExit,
    CancelExit,

    // ── Request screen — navigation ───────────────────────────────────────────
    FocusNextField,
    FocusPreviousField,
    EditFocusedField,
    EditSelectedHeader,
    ScrollDown,
    ScrollUp,
    PageDown,
    PageUp,
    GoBack,
    AddHeader,
    DeleteHeader,
    SelectNextHeader,
    SelectPreviousHeader,
    ToggleMethod,
    JumpToUrl,
    FocusHeaders,
    JumpToBody,
    SendRequest,
    CycleViewMode,
    EditJqFilter,

    // ── Editing mode ──────────────────────────────────────────────────────────
    CancelEdit,
    ConfirmEdit,
    ToggleHeaderKeyValue,
    InsertNewline,
    SaveBody,
    DeleteChar,
    AutocompleteDown,
    AutocompleteUp,
}

// ── Trigger ───────────────────────────────────────────────────────────────────

/// A key combination that can trigger an action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyTrigger {
    Char(char),
    Code(KeyCode),
    Modified(KeyModifiers, KeyCode),
}

impl KeyTrigger {
    pub fn matches(&self, event: &KeyEvent) -> bool {
        match self {
            KeyTrigger::Char(c) => {
                event.code == KeyCode::Char(*c) && event.modifiers == KeyModifiers::NONE
            }
            // For non-character key codes (Enter, Esc, Tab, BackTab, arrows, etc.)
            // we match on the code alone and ignore modifiers.  Crossterm encodes
            // the shift semantics directly into the KeyCode (e.g. BackTab already
            // implies Shift), so requiring NONE would break BackTab on most terminals.
            KeyTrigger::Code(code) => event.code == *code,
            KeyTrigger::Modified(mods, code) => {
                event.code == *code && event.modifiers.contains(*mods)
            }
        }
    }
}

// ── Binding ───────────────────────────────────────────────────────────────────

/// A single logical keybinding: one or more triggers that all fire the same
/// action, plus display strings for help rendering.
pub struct Binding {
    /// All key triggers that fire this action.
    pub triggers: Vec<KeyTrigger>,
    pub action: Action,
    /// Short display string shown in the footer/title, e.g. `"↓↑/jk"`.
    pub hint: &'static str,
    /// Verb phrase shown after the hint, e.g. `"scroll"`.
    pub description: &'static str,
}

// ── Context match helper ──────────────────────────────────────────────────────

/// How specific a context rule is. Used to pick the most specific match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Specificity {
    /// Matches any editing field in Request screen (wildcard focus).
    AnyEditing = 0,
    /// Matches a specific editing field.
    SpecificEditing = 1,
    /// Matches navigation mode with any focus.
    AnyNavigation = 2,
    /// Matches navigation mode with a specific focus.
    SpecificNavigation = 3,
}

struct ContextRule {
    screen: CurrentScreen,
    /// `None` = navigation mode, `Some(None)` = any editing field,
    /// `Some(Some(f))` = specific editing field.
    editing: Option<Option<EditingField>>,
    /// `None` = any focused field, `Some(f)` = specific focused field.
    focus: Option<FocusableField>,
    bindings: Vec<Binding>,
}

impl ContextRule {
    fn matches(&self, ctx: &KeyContext) -> Option<Specificity> {
        if self.screen != ctx.screen {
            return None;
        }
        match (self.editing, ctx.editing) {
            // Rule requires navigation mode; context is editing → no match
            (None, Some(_)) => return None,
            // Rule requires editing mode; context is navigation → no match
            (Some(_), None) => return None,
            // Rule requires a specific editing field; context must match
            (Some(Some(required)), Some(actual)) if required != actual => return None,
            _ => {}
        }
        // Check focus filter
        if let Some(required_focus) = self.focus {
            if required_focus != ctx.focus {
                return None;
            }
        }
        // Compute specificity
        let spec = match (self.editing, self.focus) {
            (None, None) => Specificity::AnyNavigation,
            (None, Some(_)) => Specificity::SpecificNavigation,
            (Some(None), _) => Specificity::AnyEditing,
            (Some(Some(_)), _) => Specificity::SpecificEditing,
        };
        Some(spec)
    }
}

// ── Keymap ────────────────────────────────────────────────────────────────────

pub struct Keymap {
    rules: Vec<ContextRule>,
}

impl Keymap {
    /// Resolve a key event to an action given the current context.
    /// The most specific matching rule wins; within a rule, earlier bindings
    /// take precedence.
    pub fn resolve(&self, ctx: &KeyContext, event: &KeyEvent) -> Option<Action> {
        // Collect all matching rules, sorted by descending specificity
        let mut candidates: Vec<(Specificity, &ContextRule)> = self
            .rules
            .iter()
            .filter_map(|rule| rule.matches(ctx).map(|spec| (spec, rule)))
            .collect();
        candidates.sort_by(|a, b| b.0.cmp(&a.0));

        // Iterate from most specific to least specific; return first match
        for (_, rule) in &candidates {
            for binding in &rule.bindings {
                for trigger in &binding.triggers {
                    // Ctrl+C is a special case — it uses CONTROL modifier but
                    // crossterm may report it as Char('c') with CONTROL mod.
                    if trigger.matches(event) {
                        return Some(binding.action);
                    }
                }
            }
        }
        None
    }

    /// Return all bindings active in a given context (for hint rendering).
    ///
    /// Returns bindings from all matching rules deduplicated by action — the
    /// most specific binding for each action wins.
    pub fn bindings_for<'a>(&'a self, ctx: &KeyContext) -> Vec<&'a Binding> {
        let mut candidates: Vec<(Specificity, &ContextRule)> = self
            .rules
            .iter()
            .filter_map(|rule| rule.matches(ctx).map(|spec| (spec, rule)))
            .collect();
        candidates.sort_by(|a, b| b.0.cmp(&a.0));

        let mut seen_actions: Vec<Action> = Vec::new();
        let mut result: Vec<&Binding> = Vec::new();

        for (_, rule) in &candidates {
            for binding in &rule.bindings {
                if !seen_actions.contains(&binding.action) {
                    seen_actions.push(binding.action);
                    result.push(binding);
                }
            }
        }
        result
    }

    /// Like `bindings_for` but returns only bindings from field-specific rules
    /// (i.e. rules with `SpecificNavigation` or `SpecificEditing` specificity).
    ///
    /// Used by widget titles to show only the shortcuts that are unique to
    /// that field, not the global navigation shortcuts.
    pub fn field_bindings_for<'a>(&'a self, ctx: &KeyContext) -> Vec<&'a Binding> {
        let specific_rules: Vec<&ContextRule> = self
            .rules
            .iter()
            .filter(|rule| {
                matches!(
                    rule.matches(ctx),
                    Some(Specificity::SpecificNavigation | Specificity::SpecificEditing)
                )
            })
            .collect();

        let mut seen_actions: Vec<Action> = Vec::new();
        let mut result: Vec<&Binding> = Vec::new();

        for rule in &specific_rules {
            for binding in &rule.bindings {
                if !seen_actions.contains(&binding.action) {
                    seen_actions.push(binding.action);
                    result.push(binding);
                }
            }
        }
        result
    }

    /// Format hint lines for the footer given the current context.
    ///
    /// Returns a list of hint-string segments that can be joined with `" | "`.
    pub fn format_hint_line(&self, ctx: &KeyContext) -> String {
        self.bindings_for(ctx)
            .iter()
            .map(|b| format!("{} - {}", b.hint, b.description))
            .collect::<Vec<_>>()
            .join(" | ")
    }

    /// Return bindings that include any trigger that is a focus-jump shortcut
    /// (i.e. `Char('u')`, `Char('h')`, `Char('b')`, etc.) for a specific
    /// field — used to show focus-shortcut hints in unfocused widget titles.
    ///
    /// Returns `(hint, description)` pairs.
    pub fn focus_shortcut_for_field(
        &self,
        field: FocusableField,
    ) -> Vec<(&'static str, &'static str)> {
        // We look in the Request-screen navigation rules (any focus) for
        // actions that jump to this specific field.
        let jump_action = match field {
            FocusableField::Url => Some(Action::JumpToUrl),
            FocusableField::Headers => Some(Action::FocusHeaders),
            FocusableField::Body => Some(Action::JumpToBody),
            FocusableField::RequestEvents => None,
            FocusableField::Response => None,
        };
        let Some(target_action) = jump_action else {
            return Vec::new();
        };
        let nav_ctx = KeyContext {
            screen: CurrentScreen::Request,
            editing: None,
            focus: FocusableField::Url, // any nav focus, doesn't matter
        };
        self.bindings_for(&nav_ctx)
            .iter()
            .filter(|b| b.action == target_action)
            .map(|b| (b.hint, b.description))
            .collect()
    }

    // ── Default keymap builder ─────────────────────────────────────────────────

    pub fn default() -> Self {
        let mut rules: Vec<ContextRule> = Vec::new();

        // ── Main screen — navigation ──────────────────────────────────────────
        rules.push(ContextRule {
            screen: CurrentScreen::Main,
            editing: None,
            focus: None,
            bindings: vec![
                Binding {
                    triggers: vec![KeyTrigger::Char('j'), KeyTrigger::Code(KeyCode::Down)],
                    action: Action::SelectNextRequest,
                    hint: "↓/j",
                    description: "next",
                },
                Binding {
                    triggers: vec![KeyTrigger::Char('k'), KeyTrigger::Code(KeyCode::Up)],
                    action: Action::SelectPreviousRequest,
                    hint: "↑/k",
                    description: "prev",
                },
                Binding {
                    triggers: vec![KeyTrigger::Code(KeyCode::PageDown)],
                    action: Action::SelectNextRequest,
                    hint: "PgDn",
                    description: "next",
                },
                Binding {
                    triggers: vec![KeyTrigger::Code(KeyCode::PageUp)],
                    action: Action::SelectPreviousRequest,
                    hint: "PgUp",
                    description: "prev",
                },
                Binding {
                    triggers: vec![KeyTrigger::Char('n')],
                    action: Action::NewRequest,
                    hint: "n",
                    description: "new",
                },
                Binding {
                    triggers: vec![KeyTrigger::Char('d')],
                    action: Action::DeleteRequest,
                    hint: "d",
                    description: "delete",
                },
                Binding {
                    triggers: vec![KeyTrigger::Char('e'), KeyTrigger::Code(KeyCode::Enter)],
                    action: Action::EditRequest,
                    hint: "e/enter",
                    description: "edit",
                },
                Binding {
                    triggers: vec![KeyTrigger::Char('s')],
                    action: Action::SaveRequests,
                    hint: "s",
                    description: "save",
                },
                Binding {
                    triggers: vec![
                        KeyTrigger::Char('q'),
                        KeyTrigger::Code(KeyCode::Backspace),
                        KeyTrigger::Modified(KeyModifiers::CONTROL, KeyCode::Char('c')),
                    ],
                    action: Action::TriggerExit,
                    hint: "q/⌫/^C",
                    description: "quit",
                },
            ],
        });

        // ── Exit confirmation ─────────────────────────────────────────────────
        rules.push(ContextRule {
            screen: CurrentScreen::Exiting,
            editing: None,
            focus: None,
            bindings: vec![
                Binding {
                    triggers: vec![
                        KeyTrigger::Char('y'),
                        KeyTrigger::Code(KeyCode::Enter),
                        KeyTrigger::Modified(KeyModifiers::CONTROL, KeyCode::Char('c')),
                    ],
                    action: Action::ConfirmExit,
                    hint: "y/enter/^C",
                    description: "yes",
                },
                Binding {
                    triggers: vec![
                        KeyTrigger::Char('n'),
                        KeyTrigger::Char('q'),
                        KeyTrigger::Code(KeyCode::Esc),
                        KeyTrigger::Code(KeyCode::Backspace),
                    ],
                    action: Action::CancelExit,
                    hint: "n/q/esc/⌫",
                    description: "no",
                },
            ],
        });

        // ── Request screen — navigation, any focus ────────────────────────────
        // These bindings are active in navigation mode regardless of focused field.
        rules.push(ContextRule {
            screen: CurrentScreen::Request,
            editing: None,
            focus: None,
            bindings: vec![
                Binding {
                    triggers: vec![KeyTrigger::Code(KeyCode::Tab)],
                    action: Action::FocusNextField,
                    hint: "tab",
                    description: "next field",
                },
                Binding {
                    triggers: vec![KeyTrigger::Code(KeyCode::BackTab)],
                    action: Action::FocusPreviousField,
                    hint: "⇧tab",
                    description: "prev field",
                },
                Binding {
                    triggers: vec![
                        KeyTrigger::Char('q'),
                        KeyTrigger::Code(KeyCode::Esc),
                        KeyTrigger::Code(KeyCode::Backspace),
                    ],
                    action: Action::GoBack,
                    hint: "q/esc/⌫",
                    description: "back",
                },
                Binding {
                    triggers: vec![KeyTrigger::Modified(
                        KeyModifiers::CONTROL,
                        KeyCode::Char('c'),
                    )],
                    action: Action::TriggerExit,
                    hint: "^C",
                    description: "quit",
                },
                Binding {
                    triggers: vec![KeyTrigger::Char('m')],
                    action: Action::ToggleMethod,
                    hint: "m",
                    description: "method",
                },
                Binding {
                    triggers: vec![KeyTrigger::Char('s')],
                    action: Action::SendRequest,
                    hint: "s",
                    description: "send",
                },
                Binding {
                    triggers: vec![KeyTrigger::Char('v')],
                    action: Action::CycleViewMode,
                    hint: "v",
                    description: "view mode",
                },
                Binding {
                    triggers: vec![KeyTrigger::Char('u')],
                    action: Action::JumpToUrl,
                    hint: "u",
                    description: "edit URL",
                },
                Binding {
                    triggers: vec![KeyTrigger::Char('h')],
                    action: Action::FocusHeaders,
                    hint: "h",
                    description: "headers",
                },
                Binding {
                    triggers: vec![KeyTrigger::Char('b')],
                    action: Action::JumpToBody,
                    hint: "b",
                    description: "edit body",
                },
            ],
        });

        // ── Request — navigation, scrollable (any focus that has scroll) ──────
        // j/k/arrows/pgup/pgdown for scroll; overridden by header-focused rules below.
        for &focus in &[
            FocusableField::Body,
            FocusableField::RequestEvents,
            FocusableField::Response,
        ] {
            rules.push(ContextRule {
                screen: CurrentScreen::Request,
                editing: None,
                focus: Some(focus),
                bindings: vec![
                    Binding {
                        triggers: vec![KeyTrigger::Char('j'), KeyTrigger::Code(KeyCode::Down)],
                        action: Action::ScrollDown,
                        hint: "↓/j",
                        description: "scroll down",
                    },
                    Binding {
                        triggers: vec![KeyTrigger::Char('k'), KeyTrigger::Code(KeyCode::Up)],
                        action: Action::ScrollUp,
                        hint: "↑/k",
                        description: "scroll up",
                    },
                    Binding {
                        triggers: vec![KeyTrigger::Code(KeyCode::PageDown)],
                        action: Action::PageDown,
                        hint: "PgDn",
                        description: "page down",
                    },
                    Binding {
                        triggers: vec![KeyTrigger::Code(KeyCode::PageUp)],
                        action: Action::PageUp,
                        hint: "PgUp",
                        description: "page up",
                    },
                ],
            });
        }

        // Url focus: e/enter to edit
        rules.push(ContextRule {
            screen: CurrentScreen::Request,
            editing: None,
            focus: Some(FocusableField::Url),
            bindings: vec![Binding {
                triggers: vec![KeyTrigger::Char('e'), KeyTrigger::Code(KeyCode::Enter)],
                action: Action::EditFocusedField,
                hint: "e/enter",
                description: "edit",
            }],
        });

        // Body focus: e/enter to edit + scroll
        rules.push(ContextRule {
            screen: CurrentScreen::Request,
            editing: None,
            focus: Some(FocusableField::Body),
            bindings: vec![Binding {
                triggers: vec![KeyTrigger::Char('e'), KeyTrigger::Code(KeyCode::Enter)],
                action: Action::EditFocusedField,
                hint: "e/enter",
                description: "edit",
            }],
        });

        // ── Request — navigation, Headers focused ─────────────────────────────
        rules.push(ContextRule {
            screen: CurrentScreen::Request,
            editing: None,
            focus: Some(FocusableField::Headers),
            bindings: vec![
                Binding {
                    triggers: vec![KeyTrigger::Char('j'), KeyTrigger::Code(KeyCode::Down)],
                    action: Action::SelectNextHeader,
                    hint: "↓/j",
                    description: "next header",
                },
                Binding {
                    triggers: vec![KeyTrigger::Char('k'), KeyTrigger::Code(KeyCode::Up)],
                    action: Action::SelectPreviousHeader,
                    hint: "↑/k",
                    description: "prev header",
                },
                Binding {
                    triggers: vec![KeyTrigger::Code(KeyCode::PageDown)],
                    action: Action::SelectNextHeader,
                    hint: "PgDn",
                    description: "next header",
                },
                Binding {
                    triggers: vec![KeyTrigger::Code(KeyCode::PageUp)],
                    action: Action::SelectPreviousHeader,
                    hint: "PgUp",
                    description: "prev header",
                },
                Binding {
                    triggers: vec![KeyTrigger::Char('e'), KeyTrigger::Code(KeyCode::Enter)],
                    action: Action::EditSelectedHeader,
                    hint: "e/enter",
                    description: "edit",
                },
                Binding {
                    triggers: vec![KeyTrigger::Char('a')],
                    action: Action::AddHeader,
                    hint: "a",
                    description: "add",
                },
                Binding {
                    triggers: vec![KeyTrigger::Char('d')],
                    action: Action::DeleteHeader,
                    hint: "d",
                    description: "delete",
                },
            ],
        });

        // ── Request — navigation, Response focused ────────────────────────────
        // f for jq filter; only active when response is focused (shown only in
        // that context, but we always register it — the execute_action handler
        // guards on json mode).
        rules.push(ContextRule {
            screen: CurrentScreen::Request,
            editing: None,
            focus: Some(FocusableField::Response),
            bindings: vec![Binding {
                triggers: vec![KeyTrigger::Char('f')],
                action: Action::EditJqFilter,
                hint: "f",
                description: "jq filter",
            }],
        });

        // ── Editing mode — any field ──────────────────────────────────────────
        rules.push(ContextRule {
            screen: CurrentScreen::Request,
            editing: Some(None), // any editing field
            focus: None,
            bindings: vec![
                Binding {
                    triggers: vec![
                        KeyTrigger::Code(KeyCode::Esc),
                        KeyTrigger::Modified(KeyModifiers::CONTROL, KeyCode::Char('c')),
                    ],
                    action: Action::CancelEdit,
                    hint: "esc/^C",
                    description: "cancel",
                },
                Binding {
                    triggers: vec![KeyTrigger::Code(KeyCode::Backspace)],
                    action: Action::DeleteChar,
                    hint: "⌫",
                    description: "delete char",
                },
            ],
        });

        // ── Editing mode — specific fields ────────────────────────────────────

        // URL editing
        rules.push(ContextRule {
            screen: CurrentScreen::Request,
            editing: Some(Some(EditingField::Url)),
            focus: None,
            bindings: vec![Binding {
                triggers: vec![KeyTrigger::Code(KeyCode::Enter)],
                action: Action::ConfirmEdit,
                hint: "enter",
                description: "confirm",
            }],
        });

        // Body editing
        rules.push(ContextRule {
            screen: CurrentScreen::Request,
            editing: Some(Some(EditingField::Body)),
            focus: None,
            bindings: vec![
                Binding {
                    triggers: vec![KeyTrigger::Code(KeyCode::Enter)],
                    action: Action::InsertNewline,
                    hint: "enter",
                    description: "newline",
                },
                Binding {
                    triggers: vec![KeyTrigger::Modified(
                        KeyModifiers::CONTROL,
                        KeyCode::Char('s'),
                    )],
                    action: Action::SaveBody,
                    hint: "^S",
                    description: "save",
                },
            ],
        });

        // JsonFilter editing
        rules.push(ContextRule {
            screen: CurrentScreen::Request,
            editing: Some(Some(EditingField::JsonFilter)),
            focus: None,
            bindings: vec![Binding {
                triggers: vec![KeyTrigger::Code(KeyCode::Enter)],
                action: Action::ConfirmEdit,
                hint: "enter",
                description: "apply",
            }],
        });

        // Headers editing
        rules.push(ContextRule {
            screen: CurrentScreen::Request,
            editing: Some(Some(EditingField::Headers)),
            focus: None,
            bindings: vec![
                Binding {
                    triggers: vec![
                        KeyTrigger::Code(KeyCode::Tab),
                        KeyTrigger::Code(KeyCode::BackTab),
                    ],
                    action: Action::ToggleHeaderKeyValue,
                    hint: "tab/⇧tab",
                    description: "toggle key/value",
                },
                Binding {
                    triggers: vec![KeyTrigger::Code(KeyCode::Enter)],
                    action: Action::ConfirmEdit,
                    hint: "enter",
                    description: "confirm",
                },
                Binding {
                    triggers: vec![KeyTrigger::Code(KeyCode::Down)],
                    action: Action::AutocompleteDown,
                    hint: "↓",
                    description: "next suggestion",
                },
                Binding {
                    triggers: vec![KeyTrigger::Code(KeyCode::Up)],
                    action: Action::AutocompleteUp,
                    hint: "↑",
                    description: "prev suggestion",
                },
            ],
        });

        Keymap { rules }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyEventKind, KeyEventState};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn char_key(c: char) -> KeyEvent {
        key(KeyCode::Char(c))
    }

    fn nav_ctx(screen: CurrentScreen, focus: FocusableField) -> KeyContext {
        KeyContext {
            screen,
            editing: None,
            focus,
        }
    }

    fn editing_ctx(field: EditingField) -> KeyContext {
        KeyContext {
            screen: CurrentScreen::Request,
            editing: Some(field),
            focus: FocusableField::Url,
        }
    }

    fn exiting_ctx() -> KeyContext {
        KeyContext {
            screen: CurrentScreen::Exiting,
            editing: None,
            focus: FocusableField::Url,
        }
    }

    // ── Main screen ───────────────────────────────────────────────────────────

    #[test]
    fn main_j_selects_next() {
        let km = Keymap::default();
        let ctx = nav_ctx(CurrentScreen::Main, FocusableField::Url);
        assert_eq!(
            km.resolve(&ctx, &char_key('j')),
            Some(Action::SelectNextRequest)
        );
        assert_eq!(
            km.resolve(&ctx, &key(KeyCode::Down)),
            Some(Action::SelectNextRequest)
        );
    }

    #[test]
    fn main_k_selects_previous() {
        let km = Keymap::default();
        let ctx = nav_ctx(CurrentScreen::Main, FocusableField::Url);
        assert_eq!(
            km.resolve(&ctx, &char_key('k')),
            Some(Action::SelectPreviousRequest)
        );
        assert_eq!(
            km.resolve(&ctx, &key(KeyCode::Up)),
            Some(Action::SelectPreviousRequest)
        );
    }

    #[test]
    fn main_pagedown_selects_next() {
        let km = Keymap::default();
        let ctx = nav_ctx(CurrentScreen::Main, FocusableField::Url);
        assert_eq!(
            km.resolve(&ctx, &key(KeyCode::PageDown)),
            Some(Action::SelectNextRequest)
        );
    }

    #[test]
    fn main_pageup_selects_previous() {
        let km = Keymap::default();
        let ctx = nav_ctx(CurrentScreen::Main, FocusableField::Url);
        assert_eq!(
            km.resolve(&ctx, &key(KeyCode::PageUp)),
            Some(Action::SelectPreviousRequest)
        );
    }

    #[test]
    fn main_q_triggers_exit() {
        let km = Keymap::default();
        let ctx = nav_ctx(CurrentScreen::Main, FocusableField::Url);
        assert_eq!(km.resolve(&ctx, &char_key('q')), Some(Action::TriggerExit));
    }

    #[test]
    fn main_backspace_triggers_exit() {
        let km = Keymap::default();
        let ctx = nav_ctx(CurrentScreen::Main, FocusableField::Url);
        assert_eq!(
            km.resolve(&ctx, &key(KeyCode::Backspace)),
            Some(Action::TriggerExit)
        );
    }

    #[test]
    fn main_ctrl_c_triggers_exit() {
        let km = Keymap::default();
        let ctx = nav_ctx(CurrentScreen::Main, FocusableField::Url);
        assert_eq!(km.resolve(&ctx, &ctrl('c')), Some(Action::TriggerExit));
    }

    // ── Exiting screen ────────────────────────────────────────────────────────

    #[test]
    fn exiting_y_confirms() {
        let km = Keymap::default();
        assert_eq!(
            km.resolve(&exiting_ctx(), &char_key('y')),
            Some(Action::ConfirmExit)
        );
    }

    #[test]
    fn exiting_enter_confirms() {
        let km = Keymap::default();
        assert_eq!(
            km.resolve(&exiting_ctx(), &key(KeyCode::Enter)),
            Some(Action::ConfirmExit)
        );
    }

    #[test]
    fn exiting_ctrl_c_confirms() {
        let km = Keymap::default();
        assert_eq!(
            km.resolve(&exiting_ctx(), &ctrl('c')),
            Some(Action::ConfirmExit)
        );
    }

    #[test]
    fn exiting_n_cancels() {
        let km = Keymap::default();
        assert_eq!(
            km.resolve(&exiting_ctx(), &char_key('n')),
            Some(Action::CancelExit)
        );
    }

    #[test]
    fn exiting_backspace_cancels() {
        let km = Keymap::default();
        assert_eq!(
            km.resolve(&exiting_ctx(), &key(KeyCode::Backspace)),
            Some(Action::CancelExit)
        );
    }

    #[test]
    fn exiting_esc_cancels() {
        let km = Keymap::default();
        assert_eq!(
            km.resolve(&exiting_ctx(), &key(KeyCode::Esc)),
            Some(Action::CancelExit)
        );
    }

    // ── Request nav — global ──────────────────────────────────────────────────

    #[test]
    fn request_tab_focuses_next() {
        let km = Keymap::default();
        let ctx = nav_ctx(CurrentScreen::Request, FocusableField::Url);
        assert_eq!(
            km.resolve(&ctx, &key(KeyCode::Tab)),
            Some(Action::FocusNextField)
        );
    }

    #[test]
    fn request_backtab_focuses_previous() {
        let km = Keymap::default();
        let ctx = nav_ctx(CurrentScreen::Request, FocusableField::Url);
        assert_eq!(
            km.resolve(&ctx, &key(KeyCode::BackTab)),
            Some(Action::FocusPreviousField)
        );
    }

    #[test]
    fn request_q_goes_back() {
        let km = Keymap::default();
        let ctx = nav_ctx(CurrentScreen::Request, FocusableField::Body);
        assert_eq!(km.resolve(&ctx, &char_key('q')), Some(Action::GoBack));
    }

    #[test]
    fn request_esc_goes_back() {
        let km = Keymap::default();
        let ctx = nav_ctx(CurrentScreen::Request, FocusableField::Body);
        assert_eq!(km.resolve(&ctx, &key(KeyCode::Esc)), Some(Action::GoBack));
    }

    #[test]
    fn request_backspace_goes_back() {
        let km = Keymap::default();
        let ctx = nav_ctx(CurrentScreen::Request, FocusableField::Body);
        assert_eq!(
            km.resolve(&ctx, &key(KeyCode::Backspace)),
            Some(Action::GoBack)
        );
    }

    #[test]
    fn request_ctrl_c_triggers_exit() {
        let km = Keymap::default();
        let ctx = nav_ctx(CurrentScreen::Request, FocusableField::Body);
        assert_eq!(km.resolve(&ctx, &ctrl('c')), Some(Action::TriggerExit));
    }

    // ── Request nav — scrollable fields ──────────────────────────────────────

    #[test]
    fn body_j_scrolls_down() {
        let km = Keymap::default();
        let ctx = nav_ctx(CurrentScreen::Request, FocusableField::Body);
        assert_eq!(km.resolve(&ctx, &char_key('j')), Some(Action::ScrollDown));
        assert_eq!(
            km.resolve(&ctx, &key(KeyCode::Down)),
            Some(Action::ScrollDown)
        );
    }

    #[test]
    fn body_k_scrolls_up() {
        let km = Keymap::default();
        let ctx = nav_ctx(CurrentScreen::Request, FocusableField::Body);
        assert_eq!(km.resolve(&ctx, &char_key('k')), Some(Action::ScrollUp));
    }

    #[test]
    fn body_pagedown_pages() {
        let km = Keymap::default();
        let ctx = nav_ctx(CurrentScreen::Request, FocusableField::Body);
        assert_eq!(
            km.resolve(&ctx, &key(KeyCode::PageDown)),
            Some(Action::PageDown)
        );
    }

    #[test]
    fn response_j_scrolls_down() {
        let km = Keymap::default();
        let ctx = nav_ctx(CurrentScreen::Request, FocusableField::Response);
        assert_eq!(km.resolve(&ctx, &char_key('j')), Some(Action::ScrollDown));
    }

    // ── Request nav — Headers focused ─────────────────────────────────────────

    #[test]
    fn headers_j_selects_next_header() {
        let km = Keymap::default();
        let ctx = nav_ctx(CurrentScreen::Request, FocusableField::Headers);
        assert_eq!(
            km.resolve(&ctx, &char_key('j')),
            Some(Action::SelectNextHeader)
        );
        assert_eq!(
            km.resolve(&ctx, &key(KeyCode::Down)),
            Some(Action::SelectNextHeader)
        );
    }

    #[test]
    fn headers_k_selects_prev_header() {
        let km = Keymap::default();
        let ctx = nav_ctx(CurrentScreen::Request, FocusableField::Headers);
        assert_eq!(
            km.resolve(&ctx, &char_key('k')),
            Some(Action::SelectPreviousHeader)
        );
    }

    #[test]
    fn headers_a_adds_header() {
        let km = Keymap::default();
        let ctx = nav_ctx(CurrentScreen::Request, FocusableField::Headers);
        assert_eq!(km.resolve(&ctx, &char_key('a')), Some(Action::AddHeader));
    }

    #[test]
    fn headers_d_deletes_header() {
        let km = Keymap::default();
        let ctx = nav_ctx(CurrentScreen::Request, FocusableField::Headers);
        assert_eq!(km.resolve(&ctx, &char_key('d')), Some(Action::DeleteHeader));
    }

    #[test]
    fn headers_enter_edits_selected() {
        let km = Keymap::default();
        let ctx = nav_ctx(CurrentScreen::Request, FocusableField::Headers);
        assert_eq!(
            km.resolve(&ctx, &key(KeyCode::Enter)),
            Some(Action::EditSelectedHeader)
        );
    }

    // ── Response focused ──────────────────────────────────────────────────────

    #[test]
    fn response_f_edits_jq_filter() {
        let km = Keymap::default();
        let ctx = nav_ctx(CurrentScreen::Request, FocusableField::Response);
        assert_eq!(km.resolve(&ctx, &char_key('f')), Some(Action::EditJqFilter));
    }

    // ── Editing mode ──────────────────────────────────────────────────────────

    #[test]
    fn editing_esc_cancels() {
        let km = Keymap::default();
        assert_eq!(
            km.resolve(&editing_ctx(EditingField::Url), &key(KeyCode::Esc)),
            Some(Action::CancelEdit)
        );
    }

    #[test]
    fn editing_ctrl_c_cancels() {
        let km = Keymap::default();
        assert_eq!(
            km.resolve(&editing_ctx(EditingField::Body), &ctrl('c')),
            Some(Action::CancelEdit)
        );
    }

    #[test]
    fn editing_backspace_deletes_char() {
        let km = Keymap::default();
        assert_eq!(
            km.resolve(&editing_ctx(EditingField::Url), &key(KeyCode::Backspace)),
            Some(Action::DeleteChar)
        );
    }

    #[test]
    fn url_enter_confirms() {
        let km = Keymap::default();
        assert_eq!(
            km.resolve(&editing_ctx(EditingField::Url), &key(KeyCode::Enter)),
            Some(Action::ConfirmEdit)
        );
    }

    #[test]
    fn body_enter_inserts_newline() {
        let km = Keymap::default();
        assert_eq!(
            km.resolve(&editing_ctx(EditingField::Body), &key(KeyCode::Enter)),
            Some(Action::InsertNewline)
        );
    }

    #[test]
    fn body_ctrl_s_saves() {
        let km = Keymap::default();
        assert_eq!(
            km.resolve(&editing_ctx(EditingField::Body), &ctrl('s')),
            Some(Action::SaveBody)
        );
    }

    #[test]
    fn headers_tab_toggles_key_value() {
        let km = Keymap::default();
        assert_eq!(
            km.resolve(&editing_ctx(EditingField::Headers), &key(KeyCode::Tab)),
            Some(Action::ToggleHeaderKeyValue)
        );
    }

    #[test]
    fn headers_down_autocomplete() {
        let km = Keymap::default();
        assert_eq!(
            km.resolve(&editing_ctx(EditingField::Headers), &key(KeyCode::Down)),
            Some(Action::AutocompleteDown)
        );
    }

    // ── bindings_for deduplication ────────────────────────────────────────────

    #[test]
    fn bindings_for_body_no_duplicate_scroll_actions() {
        let km = Keymap::default();
        let ctx = nav_ctx(CurrentScreen::Request, FocusableField::Body);
        let bindings = km.bindings_for(&ctx);
        let scroll_down_count = bindings
            .iter()
            .filter(|b| b.action == Action::ScrollDown)
            .count();
        assert_eq!(scroll_down_count, 1, "ScrollDown should appear only once");
    }

    // ── format_hint_line ──────────────────────────────────────────────────────

    #[test]
    fn hint_line_for_main_includes_quit() {
        let km = Keymap::default();
        let ctx = nav_ctx(CurrentScreen::Main, FocusableField::Url);
        let hints = km.format_hint_line(&ctx);
        assert!(hints.contains("quit"), "expected 'quit' in: {hints}");
    }

    #[test]
    fn hint_line_for_request_body_includes_scroll() {
        let km = Keymap::default();
        let ctx = nav_ctx(CurrentScreen::Request, FocusableField::Body);
        let hints = km.format_hint_line(&ctx);
        assert!(hints.contains("scroll"), "expected 'scroll' in: {hints}");
    }

    // ── focus_shortcut_for_field ──────────────────────────────────────────────

    #[test]
    fn focus_shortcut_for_url() {
        let km = Keymap::default();
        let shortcuts = km.focus_shortcut_for_field(FocusableField::Url);
        assert!(!shortcuts.is_empty());
        assert!(shortcuts.iter().any(|(hint, _)| hint.contains('u')));
    }

    #[test]
    fn focus_shortcut_for_headers() {
        let km = Keymap::default();
        let shortcuts = km.focus_shortcut_for_field(FocusableField::Headers);
        assert!(!shortcuts.is_empty());
        assert!(shortcuts.iter().any(|(hint, _)| hint.contains('h')));
    }

    #[test]
    fn focus_shortcut_for_events_is_empty() {
        let km = Keymap::default();
        let shortcuts = km.focus_shortcut_for_field(FocusableField::RequestEvents);
        assert!(shortcuts.is_empty());
    }

    // ── field_bindings_for ────────────────────────────────────────────────────

    /// When Body is focused, field_bindings_for should return scroll bindings
    /// but NOT global ones like Tab or q/Esc (GoBack).
    #[test]
    fn field_bindings_for_body_includes_scroll_not_global() {
        let km = Keymap::default();
        let ctx = nav_ctx(CurrentScreen::Request, FocusableField::Body);
        let bindings = km.field_bindings_for(&ctx);
        let actions: Vec<Action> = bindings.iter().map(|b| b.action).collect();
        assert!(
            actions.contains(&Action::ScrollDown),
            "expected ScrollDown in field bindings for Body"
        );
        assert!(
            !actions.contains(&Action::FocusNextField),
            "Tab (FocusNextField) should not appear in field-only bindings"
        );
        assert!(
            !actions.contains(&Action::GoBack),
            "GoBack should not appear in field-only bindings"
        );
    }

    /// When Headers is focused, field_bindings_for returns header-specific
    /// actions (AddHeader, DeleteHeader, etc.) but not global ones.
    #[test]
    fn field_bindings_for_headers_includes_header_actions_not_global() {
        let km = Keymap::default();
        let ctx = nav_ctx(CurrentScreen::Request, FocusableField::Headers);
        let bindings = km.field_bindings_for(&ctx);
        let actions: Vec<Action> = bindings.iter().map(|b| b.action).collect();
        assert!(
            actions.contains(&Action::AddHeader),
            "expected AddHeader in field bindings for Headers"
        );
        assert!(
            actions.contains(&Action::DeleteHeader),
            "expected DeleteHeader in field bindings for Headers"
        );
        assert!(
            !actions.contains(&Action::FocusNextField),
            "Tab should not appear in field-only bindings"
        );
    }

    /// When URL is focused, field_bindings_for returns Enter-to-edit binding
    /// but NOT global ones like Tab or GoBack.
    #[test]
    fn field_bindings_for_url_nav_has_edit_not_global() {
        let km = Keymap::default();
        let ctx = nav_ctx(CurrentScreen::Request, FocusableField::Url);
        let bindings = km.field_bindings_for(&ctx);
        let actions: Vec<Action> = bindings.iter().map(|b| b.action).collect();
        assert!(
            actions.contains(&Action::EditFocusedField),
            "expected EditFocusedField in URL nav field bindings, got: {actions:?}"
        );
        assert!(
            !actions.contains(&Action::FocusNextField),
            "Tab (FocusNextField) should not appear in field-only bindings"
        );
        assert!(
            !actions.contains(&Action::GoBack),
            "GoBack should not appear in field-only bindings"
        );
    }

    /// Editing Body: field_bindings_for returns editing-mode actions
    /// (SaveBody, InsertNewline, CancelEdit) but not global nav ones.
    #[test]
    fn field_bindings_for_editing_body_includes_save_and_newline() {
        let km = Keymap::default();
        let ctx = editing_ctx(EditingField::Body);
        let bindings = km.field_bindings_for(&ctx);
        let actions: Vec<Action> = bindings.iter().map(|b| b.action).collect();
        assert!(
            actions.contains(&Action::SaveBody),
            "expected SaveBody in editing Body field bindings"
        );
        assert!(
            actions.contains(&Action::InsertNewline),
            "expected InsertNewline in editing Body field bindings"
        );
    }
}
