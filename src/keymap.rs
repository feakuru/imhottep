use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use smallvec::{smallvec, SmallVec};

use crate::app::{CurrentScreen, EditingField, FocusableField};

// ── Context ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyContext {
    pub screen: CurrentScreen,
    pub editing: Option<EditingField>,
    pub focus: FocusableField,
}

// ── Action ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    // ── Global ────────────────────────────────────────────────────────────────
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
    EditStreamPrefixRegex,
    EditStreamSuffixRegex,

    // ── Editing mode ──────────────────────────────────────────────────────────
    CancelEdit,
    ConfirmEdit,
    ToggleHeaderKeyValue,
    InsertNewline,
    SaveBody,
    DeleteChar,
    DeleteNextChar,
    DeleteWordBackward,
    DeleteWordForward,
    CursorLeft,
    CursorRight,
    CursorWordLeft,
    CursorWordRight,
    CursorHome,
    CursorEnd,
    AutocompleteDown,
    AutocompleteUp,
}

// ── Trigger ───────────────────────────────────────────────────────────────────

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
            KeyTrigger::Code(code) => {
                event.code == *code && event.modifiers == KeyModifiers::NONE
            }
            KeyTrigger::Modified(mods, code) => {
                event.code == *code && event.modifiers.contains(*mods)
            }
        }
    }
}

// ── Binding ───────────────────────────────────────────────────────────────────

pub struct Binding {
    pub triggers: SmallVec<[KeyTrigger; 4]>,
    pub action: Action,
    pub hint: &'static str,
    pub description: &'static str,
}

// ── Context match helper ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Specificity {
    AnyEditing = 0,
    SpecificEditing = 1,
    AnyNavigation = 2,
    SpecificNavigation = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditingMatch {
    Navigation,
    AnyField,
    SpecificField(EditingField),
}

struct ContextRule {
    screen: CurrentScreen,
    editing: EditingMatch,
    focus: Option<FocusableField>,
    bindings: Vec<Binding>,
}

impl ContextRule {
    fn matches(&self, ctx: &KeyContext) -> Option<Specificity> {
        if self.screen != ctx.screen {
            return None;
        }
        match (&self.editing, ctx.editing) {
            (EditingMatch::Navigation, Some(_)) => return None,
            (EditingMatch::AnyField | EditingMatch::SpecificField(_), None) => return None,
            (EditingMatch::SpecificField(required), Some(actual)) if *required != actual => {
                return None;
            }
            _ => {}
        }
        if let Some(required_focus) = self.focus {
            if required_focus != ctx.focus {
                return None;
            }
        }
        let spec = match (&self.editing, self.focus) {
            (EditingMatch::Navigation, None) => Specificity::AnyNavigation,
            (EditingMatch::Navigation, Some(_)) => Specificity::SpecificNavigation,
            (EditingMatch::AnyField, _) => Specificity::AnyEditing,
            (EditingMatch::SpecificField(_), _) => Specificity::SpecificEditing,
        };
        Some(spec)
    }
}

// ── Keymap ────────────────────────────────────────────────────────────────────

pub struct Keymap {
    rules: Vec<ContextRule>,
}

impl Keymap {
    pub fn resolve(&self, ctx: &KeyContext, event: &KeyEvent) -> Option<Action> {
        let mut candidates: Vec<(&ContextRule, Specificity)> = self
            .rules
            .iter()
            .filter_map(|rule| rule.matches(ctx).map(|spec| (rule, spec)))
            .collect();
        candidates.sort_by(|a, b| b.1.cmp(&a.1));

        for (rule, _) in &candidates {
            for binding in &rule.bindings {
                if binding.triggers.iter().any(|t| t.matches(event)) {
                    return Some(binding.action);
                }
            }
        }
        None
    }

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

    pub fn field_bindings_for<'a>(&'a self, ctx: &KeyContext) -> Vec<&'a Binding> {
        let mut seen_actions: Vec<Action> = Vec::new();
        let mut result: Vec<&Binding> = Vec::new();

        for rule in &self.rules {
            if matches!(
                rule.matches(ctx),
                Some(Specificity::SpecificNavigation | Specificity::SpecificEditing)
            ) {
                for binding in &rule.bindings {
                    if !seen_actions.contains(&binding.action) {
                        seen_actions.push(binding.action);
                        result.push(binding);
                    }
                }
            }
        }
        result
    }

    pub fn format_hint_line(&self, ctx: &KeyContext) -> String {
        self.bindings_for(ctx)
            .iter()
            .map(|b| format!("{} - {}", b.hint, b.description))
            .collect::<Vec<_>>()
            .join(" | ")
    }

    pub fn focus_shortcut_for_field(
        &self,
        field: FocusableField,
    ) -> Vec<(&'static str, &'static str)> {
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
            focus: FocusableField::Url,
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

        rules.push(ContextRule {
            screen: CurrentScreen::Main,
            editing: EditingMatch::Navigation,
            focus: None,
            bindings: vec![
                Binding {
                    triggers: smallvec![KeyTrigger::Char('j'), KeyTrigger::Code(KeyCode::Down)],
                    action: Action::SelectNextRequest,
                    hint: "↓/j",
                    description: "next",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Char('k'), KeyTrigger::Code(KeyCode::Up)],
                    action: Action::SelectPreviousRequest,
                    hint: "↑/k",
                    description: "prev",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::PageDown)],
                    action: Action::SelectNextRequest,
                    hint: "PgDn",
                    description: "next",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::PageUp)],
                    action: Action::SelectPreviousRequest,
                    hint: "PgUp",
                    description: "prev",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Char('n')],
                    action: Action::NewRequest,
                    hint: "n",
                    description: "new",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Char('d')],
                    action: Action::DeleteRequest,
                    hint: "d",
                    description: "delete",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Char('e'), KeyTrigger::Code(KeyCode::Enter)],
                    action: Action::EditRequest,
                    hint: "e/enter",
                    description: "edit",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Char('s')],
                    action: Action::SaveRequests,
                    hint: "s",
                    description: "save",
                },
                Binding {
                    triggers: smallvec![
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
            editing: EditingMatch::Navigation,
            focus: None,
            bindings: vec![
                Binding {
                    triggers: smallvec![
                        KeyTrigger::Char('y'),
                        KeyTrigger::Code(KeyCode::Enter),
                        KeyTrigger::Modified(KeyModifiers::CONTROL, KeyCode::Char('c')),
                    ],
                    action: Action::ConfirmExit,
                    hint: "y/enter/^C",
                    description: "yes",
                },
                Binding {
                    triggers: smallvec![
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
        rules.push(ContextRule {
            screen: CurrentScreen::Request,
            editing: EditingMatch::Navigation,
            focus: None,
            bindings: vec![
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::Tab)],
                    action: Action::FocusNextField,
                    hint: "tab",
                    description: "next field",
                },
                Binding {
                    triggers: smallvec![
                        KeyTrigger::Code(KeyCode::BackTab),
                        KeyTrigger::Modified(KeyModifiers::SHIFT, KeyCode::BackTab),
                    ],
                    action: Action::FocusPreviousField,
                    hint: "⇧tab",
                    description: "prev field",
                },
                Binding {
                    triggers: smallvec![
                        KeyTrigger::Char('q'),
                        KeyTrigger::Code(KeyCode::Esc),
                        KeyTrigger::Code(KeyCode::Backspace),
                    ],
                    action: Action::GoBack,
                    hint: "q/esc/⌫",
                    description: "back",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Modified(
                        KeyModifiers::CONTROL,
                        KeyCode::Char('c'),
                    )],
                    action: Action::TriggerExit,
                    hint: "^C",
                    description: "quit",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Char('m')],
                    action: Action::ToggleMethod,
                    hint: "m",
                    description: "method",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Char('s')],
                    action: Action::SendRequest,
                    hint: "s",
                    description: "send",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Char('v')],
                    action: Action::CycleViewMode,
                    hint: "v",
                    description: "view mode",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Char('u')],
                    action: Action::JumpToUrl,
                    hint: "u",
                    description: "edit URL",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Char('h')],
                    action: Action::FocusHeaders,
                    hint: "h",
                    description: "headers",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Char('b')],
                    action: Action::JumpToBody,
                    hint: "b",
                    description: "edit body",
                },
            ],
        });

        // ── Request — navigation, scrollable fields ───────────────────────────
        for &focus in &[
            FocusableField::Body,
            FocusableField::RequestEvents,
            FocusableField::Response,
        ] {
            rules.push(ContextRule {
                screen: CurrentScreen::Request,
                editing: EditingMatch::Navigation,
                focus: Some(focus),
                bindings: vec![
                    Binding {
                        triggers: smallvec![KeyTrigger::Char('j'), KeyTrigger::Code(KeyCode::Down)],
                        action: Action::ScrollDown,
                        hint: "↓/j",
                        description: "scroll down",
                    },
                    Binding {
                        triggers: smallvec![KeyTrigger::Char('k'), KeyTrigger::Code(KeyCode::Up)],
                        action: Action::ScrollUp,
                        hint: "↑/k",
                        description: "scroll up",
                    },
                    Binding {
                        triggers: smallvec![KeyTrigger::Code(KeyCode::PageDown)],
                        action: Action::PageDown,
                        hint: "PgDn",
                        description: "page down",
                    },
                    Binding {
                        triggers: smallvec![KeyTrigger::Code(KeyCode::PageUp)],
                        action: Action::PageUp,
                        hint: "PgUp",
                        description: "page up",
                    },
                ],
            });
        }

        // Url focus
        rules.push(ContextRule {
            screen: CurrentScreen::Request,
            editing: EditingMatch::Navigation,
            focus: Some(FocusableField::Url),
            bindings: vec![Binding {
                triggers: smallvec![KeyTrigger::Char('e'), KeyTrigger::Code(KeyCode::Enter)],
                action: Action::EditFocusedField,
                hint: "e/enter",
                description: "edit",
            }],
        });

        // Body focus
        rules.push(ContextRule {
            screen: CurrentScreen::Request,
            editing: EditingMatch::Navigation,
            focus: Some(FocusableField::Body),
            bindings: vec![Binding {
                triggers: smallvec![KeyTrigger::Char('e'), KeyTrigger::Code(KeyCode::Enter)],
                action: Action::EditFocusedField,
                hint: "e/enter",
                description: "edit",
            }],
        });

        // ── Request — navigation, Headers focused ─────────────────────────────
        rules.push(ContextRule {
            screen: CurrentScreen::Request,
            editing: EditingMatch::Navigation,
            focus: Some(FocusableField::Headers),
            bindings: vec![
                Binding {
                    triggers: smallvec![KeyTrigger::Char('j'), KeyTrigger::Code(KeyCode::Down)],
                    action: Action::SelectNextHeader,
                    hint: "↓/j",
                    description: "next header",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Char('k'), KeyTrigger::Code(KeyCode::Up)],
                    action: Action::SelectPreviousHeader,
                    hint: "↑/k",
                    description: "prev header",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::PageDown)],
                    action: Action::SelectNextHeader,
                    hint: "PgDn",
                    description: "next header",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::PageUp)],
                    action: Action::SelectPreviousHeader,
                    hint: "PgUp",
                    description: "prev header",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Char('e'), KeyTrigger::Code(KeyCode::Enter)],
                    action: Action::EditSelectedHeader,
                    hint: "e/enter",
                    description: "edit",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Char('a')],
                    action: Action::AddHeader,
                    hint: "a",
                    description: "add",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Char('d')],
                    action: Action::DeleteHeader,
                    hint: "d",
                    description: "delete",
                },
            ],
        });

        // ── Request — navigation, Response focused ────────────────────────────
        rules.push(ContextRule {
            screen: CurrentScreen::Request,
            editing: EditingMatch::Navigation,
            focus: Some(FocusableField::Response),
            bindings: vec![
                Binding {
                    triggers: smallvec![KeyTrigger::Char('f')],
                    action: Action::EditJqFilter,
                    hint: "f",
                    description: "jq filter",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Char('p')],
                    action: Action::EditStreamPrefixRegex,
                    hint: "p",
                    description: "prefix regex",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Char('x')],
                    action: Action::EditStreamSuffixRegex,
                    hint: "x",
                    description: "suffix regex",
                },
            ],
        });

        // ── Editing mode — any field ──────────────────────────────────────────
        rules.push(ContextRule {
            screen: CurrentScreen::Request,
            editing: EditingMatch::AnyField,
            focus: None,
            bindings: vec![
                Binding {
                    triggers: smallvec![
                        KeyTrigger::Code(KeyCode::Esc),
                        KeyTrigger::Modified(KeyModifiers::CONTROL, KeyCode::Char('c')),
                    ],
                    action: Action::CancelEdit,
                    hint: "esc/^C",
                    description: "cancel",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::Backspace)],
                    action: Action::DeleteChar,
                    hint: "⌫",
                    description: "delete char",
                },
            ],
        });

        // ── URL editing ────────────────────────────────────────────────────────
        rules.push(ContextRule {
            screen: CurrentScreen::Request,
            editing: EditingMatch::SpecificField(EditingField::Url),
            focus: None,
            bindings: vec![
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::Enter)],
                    action: Action::ConfirmEdit,
                    hint: "enter",
                    description: "confirm",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::Left)],
                    action: Action::CursorLeft,
                    hint: "←",
                    description: "cursor left",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::Right)],
                    action: Action::CursorRight,
                    hint: "→",
                    description: "cursor right",
                },
                Binding {
                    triggers: smallvec![
                        KeyTrigger::Modified(KeyModifiers::CONTROL, KeyCode::Left),
                        KeyTrigger::Modified(KeyModifiers::ALT, KeyCode::Left),
                    ],
                    action: Action::CursorWordLeft,
                    hint: "^←/M←",
                    description: "word left",
                },
                Binding {
                    triggers: smallvec![
                        KeyTrigger::Modified(KeyModifiers::CONTROL, KeyCode::Right),
                        KeyTrigger::Modified(KeyModifiers::ALT, KeyCode::Right),
                    ],
                    action: Action::CursorWordRight,
                    hint: "^→/M→",
                    description: "word right",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::Home)],
                    action: Action::CursorHome,
                    hint: "home",
                    description: "line start",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::End)],
                    action: Action::CursorEnd,
                    hint: "end",
                    description: "line end",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::Delete)],
                    action: Action::DeleteNextChar,
                    hint: "del",
                    description: "delete next",
                },
                Binding {
                    triggers: smallvec![
                        KeyTrigger::Modified(KeyModifiers::CONTROL, KeyCode::Backspace),
                        KeyTrigger::Modified(KeyModifiers::ALT, KeyCode::Backspace),
                        KeyTrigger::Modified(KeyModifiers::CONTROL, KeyCode::Char('h')),
                    ],
                    action: Action::DeleteWordBackward,
                    hint: "^⌫/M⌫",
                    description: "delete word",
                },
                Binding {
                    triggers: smallvec![
                        KeyTrigger::Modified(KeyModifiers::CONTROL, KeyCode::Delete),
                        KeyTrigger::Modified(KeyModifiers::ALT, KeyCode::Delete),
                    ],
                    action: Action::DeleteWordForward,
                    hint: "^⌦/M⌦",
                    description: "delete word forward",
                },
            ],
        });

        // ── Body editing ───────────────────────────────────────────────────────
        rules.push(ContextRule {
            screen: CurrentScreen::Request,
            editing: EditingMatch::SpecificField(EditingField::Body),
            focus: None,
            bindings: vec![
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::Enter)],
                    action: Action::InsertNewline,
                    hint: "enter",
                    description: "newline",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Modified(
                        KeyModifiers::CONTROL,
                        KeyCode::Char('s'),
                    )],
                    action: Action::SaveBody,
                    hint: "^S",
                    description: "save",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::Left)],
                    action: Action::CursorLeft,
                    hint: "←",
                    description: "cursor left",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::Right)],
                    action: Action::CursorRight,
                    hint: "→",
                    description: "cursor right",
                },
                Binding {
                    triggers: smallvec![
                        KeyTrigger::Modified(KeyModifiers::CONTROL, KeyCode::Left),
                        KeyTrigger::Modified(KeyModifiers::ALT, KeyCode::Left),
                    ],
                    action: Action::CursorWordLeft,
                    hint: "^←/M←",
                    description: "word left",
                },
                Binding {
                    triggers: smallvec![
                        KeyTrigger::Modified(KeyModifiers::CONTROL, KeyCode::Right),
                        KeyTrigger::Modified(KeyModifiers::ALT, KeyCode::Right),
                    ],
                    action: Action::CursorWordRight,
                    hint: "^→/M→",
                    description: "word right",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::Home)],
                    action: Action::CursorHome,
                    hint: "home",
                    description: "line start",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::End)],
                    action: Action::CursorEnd,
                    hint: "end",
                    description: "line end",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::Delete)],
                    action: Action::DeleteNextChar,
                    hint: "del",
                    description: "delete next",
                },
                Binding {
                    triggers: smallvec![
                        KeyTrigger::Modified(KeyModifiers::CONTROL, KeyCode::Backspace),
                        KeyTrigger::Modified(KeyModifiers::ALT, KeyCode::Backspace),
                        KeyTrigger::Modified(KeyModifiers::CONTROL, KeyCode::Char('h')),
                    ],
                    action: Action::DeleteWordBackward,
                    hint: "^⌫/M⌫",
                    description: "delete word",
                },
                Binding {
                    triggers: smallvec![
                        KeyTrigger::Modified(KeyModifiers::CONTROL, KeyCode::Delete),
                        KeyTrigger::Modified(KeyModifiers::ALT, KeyCode::Delete),
                    ],
                    action: Action::DeleteWordForward,
                    hint: "^⌦/M⌦",
                    description: "delete word forward",
                },
            ],
        });

        // ── JsonFilter editing ─────────────────────────────────────────────────
        rules.push(ContextRule {
            screen: CurrentScreen::Request,
            editing: EditingMatch::SpecificField(EditingField::JsonFilter),
            focus: None,
            bindings: vec![
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::Enter)],
                    action: Action::ConfirmEdit,
                    hint: "enter",
                    description: "apply",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::Left)],
                    action: Action::CursorLeft,
                    hint: "←",
                    description: "cursor left",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::Right)],
                    action: Action::CursorRight,
                    hint: "→",
                    description: "cursor right",
                },
                Binding {
                    triggers: smallvec![
                        KeyTrigger::Modified(KeyModifiers::CONTROL, KeyCode::Left),
                        KeyTrigger::Modified(KeyModifiers::ALT, KeyCode::Left),
                    ],
                    action: Action::CursorWordLeft,
                    hint: "^←/M←",
                    description: "word left",
                },
                Binding {
                    triggers: smallvec![
                        KeyTrigger::Modified(KeyModifiers::CONTROL, KeyCode::Right),
                        KeyTrigger::Modified(KeyModifiers::ALT, KeyCode::Right),
                    ],
                    action: Action::CursorWordRight,
                    hint: "^→/M→",
                    description: "word right",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::Home)],
                    action: Action::CursorHome,
                    hint: "home",
                    description: "line start",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::End)],
                    action: Action::CursorEnd,
                    hint: "end",
                    description: "line end",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::Delete)],
                    action: Action::DeleteNextChar,
                    hint: "del",
                    description: "delete next",
                },
                Binding {
                    triggers: smallvec![
                        KeyTrigger::Modified(KeyModifiers::CONTROL, KeyCode::Backspace),
                        KeyTrigger::Modified(KeyModifiers::ALT, KeyCode::Backspace),
                        KeyTrigger::Modified(KeyModifiers::CONTROL, KeyCode::Char('h')),
                    ],
                    action: Action::DeleteWordBackward,
                    hint: "^⌫/M⌫",
                    description: "delete word",
                },
                Binding {
                    triggers: smallvec![
                        KeyTrigger::Modified(KeyModifiers::CONTROL, KeyCode::Delete),
                        KeyTrigger::Modified(KeyModifiers::ALT, KeyCode::Delete),
                    ],
                    action: Action::DeleteWordForward,
                    hint: "^⌦/M⌦",
                    description: "delete word forward",
                },
            ],
        });

        // ── StreamPrefixRegex editing ──────────────────────────────────────────
        rules.push(ContextRule {
            screen: CurrentScreen::Request,
            editing: EditingMatch::SpecificField(EditingField::StreamPrefixRegex),
            focus: None,
            bindings: vec![
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::Enter)],
                    action: Action::ConfirmEdit,
                    hint: "enter",
                    description: "apply",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::Left)],
                    action: Action::CursorLeft,
                    hint: "←",
                    description: "cursor left",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::Right)],
                    action: Action::CursorRight,
                    hint: "→",
                    description: "cursor right",
                },
                Binding {
                    triggers: smallvec![
                        KeyTrigger::Modified(KeyModifiers::CONTROL, KeyCode::Left),
                        KeyTrigger::Modified(KeyModifiers::ALT, KeyCode::Left),
                    ],
                    action: Action::CursorWordLeft,
                    hint: "^←/M←",
                    description: "word left",
                },
                Binding {
                    triggers: smallvec![
                        KeyTrigger::Modified(KeyModifiers::CONTROL, KeyCode::Right),
                        KeyTrigger::Modified(KeyModifiers::ALT, KeyCode::Right),
                    ],
                    action: Action::CursorWordRight,
                    hint: "^→/M→",
                    description: "word right",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::Home)],
                    action: Action::CursorHome,
                    hint: "home",
                    description: "line start",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::End)],
                    action: Action::CursorEnd,
                    hint: "end",
                    description: "line end",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::Delete)],
                    action: Action::DeleteNextChar,
                    hint: "del",
                    description: "delete next",
                },
                Binding {
                    triggers: smallvec![
                        KeyTrigger::Modified(KeyModifiers::CONTROL, KeyCode::Backspace),
                        KeyTrigger::Modified(KeyModifiers::ALT, KeyCode::Backspace),
                        KeyTrigger::Modified(KeyModifiers::CONTROL, KeyCode::Char('h')),
                    ],
                    action: Action::DeleteWordBackward,
                    hint: "^⌫/M⌫",
                    description: "delete word",
                },
                Binding {
                    triggers: smallvec![
                        KeyTrigger::Modified(KeyModifiers::CONTROL, KeyCode::Delete),
                        KeyTrigger::Modified(KeyModifiers::ALT, KeyCode::Delete),
                    ],
                    action: Action::DeleteWordForward,
                    hint: "^⌦/M⌦",
                    description: "delete word forward",
                },
            ],
        });

        // ── StreamSuffixRegex editing ──────────────────────────────────────────
        rules.push(ContextRule {
            screen: CurrentScreen::Request,
            editing: EditingMatch::SpecificField(EditingField::StreamSuffixRegex),
            focus: None,
            bindings: vec![
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::Enter)],
                    action: Action::ConfirmEdit,
                    hint: "enter",
                    description: "apply",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::Left)],
                    action: Action::CursorLeft,
                    hint: "←",
                    description: "cursor left",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::Right)],
                    action: Action::CursorRight,
                    hint: "→",
                    description: "cursor right",
                },
                Binding {
                    triggers: smallvec![
                        KeyTrigger::Modified(KeyModifiers::CONTROL, KeyCode::Left),
                        KeyTrigger::Modified(KeyModifiers::ALT, KeyCode::Left),
                    ],
                    action: Action::CursorWordLeft,
                    hint: "^←/M←",
                    description: "word left",
                },
                Binding {
                    triggers: smallvec![
                        KeyTrigger::Modified(KeyModifiers::CONTROL, KeyCode::Right),
                        KeyTrigger::Modified(KeyModifiers::ALT, KeyCode::Right),
                    ],
                    action: Action::CursorWordRight,
                    hint: "^→/M→",
                    description: "word right",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::Home)],
                    action: Action::CursorHome,
                    hint: "home",
                    description: "line start",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::End)],
                    action: Action::CursorEnd,
                    hint: "end",
                    description: "line end",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::Delete)],
                    action: Action::DeleteNextChar,
                    hint: "del",
                    description: "delete next",
                },
                Binding {
                    triggers: smallvec![
                        KeyTrigger::Modified(KeyModifiers::CONTROL, KeyCode::Backspace),
                        KeyTrigger::Modified(KeyModifiers::ALT, KeyCode::Backspace),
                        KeyTrigger::Modified(KeyModifiers::CONTROL, KeyCode::Char('h')),
                    ],
                    action: Action::DeleteWordBackward,
                    hint: "^⌫/M⌫",
                    description: "delete word",
                },
                Binding {
                    triggers: smallvec![
                        KeyTrigger::Modified(KeyModifiers::CONTROL, KeyCode::Delete),
                        KeyTrigger::Modified(KeyModifiers::ALT, KeyCode::Delete),
                    ],
                    action: Action::DeleteWordForward,
                    hint: "^⌦/M⌦",
                    description: "delete word forward",
                },
            ],
        });

        // ── Headers editing ────────────────────────────────────────────────────
        rules.push(ContextRule {
            screen: CurrentScreen::Request,
            editing: EditingMatch::SpecificField(EditingField::Headers),
            focus: None,
            bindings: vec![
                Binding {
                    triggers: smallvec![
                        KeyTrigger::Code(KeyCode::Tab),
                        KeyTrigger::Code(KeyCode::BackTab),
                        KeyTrigger::Modified(KeyModifiers::SHIFT, KeyCode::BackTab),
                    ],
                    action: Action::ToggleHeaderKeyValue,
                    hint: "tab/⇧tab",
                    description: "toggle key/value",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::Enter)],
                    action: Action::ConfirmEdit,
                    hint: "enter",
                    description: "confirm",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::Down)],
                    action: Action::AutocompleteDown,
                    hint: "↓",
                    description: "next suggestion",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::Up)],
                    action: Action::AutocompleteUp,
                    hint: "↑",
                    description: "prev suggestion",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::Left)],
                    action: Action::CursorLeft,
                    hint: "←",
                    description: "cursor left",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::Right)],
                    action: Action::CursorRight,
                    hint: "→",
                    description: "cursor right",
                },
                Binding {
                    triggers: smallvec![
                        KeyTrigger::Modified(KeyModifiers::CONTROL, KeyCode::Left),
                        KeyTrigger::Modified(KeyModifiers::ALT, KeyCode::Left),
                    ],
                    action: Action::CursorWordLeft,
                    hint: "^←/M←",
                    description: "word left",
                },
                Binding {
                    triggers: smallvec![
                        KeyTrigger::Modified(KeyModifiers::CONTROL, KeyCode::Right),
                        KeyTrigger::Modified(KeyModifiers::ALT, KeyCode::Right),
                    ],
                    action: Action::CursorWordRight,
                    hint: "^→/M→",
                    description: "word right",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::Home)],
                    action: Action::CursorHome,
                    hint: "home",
                    description: "line start",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::End)],
                    action: Action::CursorEnd,
                    hint: "end",
                    description: "line end",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::Delete)],
                    action: Action::DeleteNextChar,
                    hint: "del",
                    description: "delete next",
                },
                Binding {
                    triggers: smallvec![
                        KeyTrigger::Modified(KeyModifiers::CONTROL, KeyCode::Backspace),
                        KeyTrigger::Modified(KeyModifiers::ALT, KeyCode::Backspace),
                        KeyTrigger::Modified(KeyModifiers::CONTROL, KeyCode::Char('h')),
                    ],
                    action: Action::DeleteWordBackward,
                    hint: "^⌫/M⌫",
                    description: "delete word",
                },
                Binding {
                    triggers: smallvec![
                        KeyTrigger::Modified(KeyModifiers::CONTROL, KeyCode::Delete),
                        KeyTrigger::Modified(KeyModifiers::ALT, KeyCode::Delete),
                    ],
                    action: Action::DeleteWordForward,
                    hint: "^⌦/M⌦",
                    description: "delete word forward",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::PageUp)],
                    action: Action::ScrollUp,
                    hint: "⇞",
                    description: "scroll up",
                },
                Binding {
                    triggers: smallvec![KeyTrigger::Code(KeyCode::PageDown)],
                    action: Action::ScrollDown,
                    hint: "⇟",
                    description: "scroll down",
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
    fn request_shift_backtab_focuses_previous() {
        let km = Keymap::default();
        let ctx = nav_ctx(CurrentScreen::Request, FocusableField::Url);
        let event = KeyEvent {
            code: KeyCode::BackTab,
            modifiers: KeyModifiers::SHIFT,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        };
        assert_eq!(
            km.resolve(&ctx, &event),
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

    #[test]
    fn response_f_edits_jq_filter() {
        let km = Keymap::default();
        let ctx = nav_ctx(CurrentScreen::Request, FocusableField::Response);
        assert_eq!(km.resolve(&ctx, &char_key('f')), Some(Action::EditJqFilter));
    }

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
