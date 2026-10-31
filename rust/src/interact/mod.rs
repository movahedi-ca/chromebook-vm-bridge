//! Deterministic interaction engine (playbook §8).
//!
//! Fixes the 52-minutes-of-stalls failure class from the Oct 30 field run:
//! React-controlled inputs, segmented date pickers, OT-editor pastes,
//! native file dialogs, and Workday multiselect widgets — all driven by
//! CDP-injected JS, never OS dialogs or keystroke floods.

pub mod autocomplete;
pub mod react;
pub mod upload;
