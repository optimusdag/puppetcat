// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 PuppetCat contributors

//! The transitions a cut can carry.
//!
//! Unlike effects, transitions are not packages: each is resolved by the
//! export crate into overlaps, opacity ramps and motion, so the list is code.
//! This is the one list; the window's cards and the agent's catalogue both
//! name these ids, and a test below fails if the window's list drifts from it.

/// One transition a clip's `transitionIn` can name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TransitionInfo {
    /// What a project file stores. Forever.
    pub id: &'static str,
    /// What the card says.
    pub name: &'static str,
    /// The shelf the card sits on.
    pub category: &'static str,
    /// One sentence on what the cut looks like.
    pub description: &'static str,
}

/// Every transition, in the order the window lists them.
pub const TRANSITIONS: [TransitionInfo; 7] = [
    TransitionInfo {
        id: "cross-fade",
        name: "Cross Fade",
        category: "Basic",
        description: "The outgoing picture dissolves into the incoming one.",
    },
    TransitionInfo {
        id: "fade-black",
        name: "Fade to Black",
        category: "Basic",
        description: "Dips to black at the cut, then up into the next clip.",
    },
    TransitionInfo {
        id: "fade-white",
        name: "Fade to White",
        category: "Basic",
        description: "Dips to white at the cut, then down into the next clip.",
    },
    TransitionInfo {
        id: "wipe-left",
        name: "Wipe Left",
        category: "Motion",
        description: "An edge sweeps leftwards, uncovering the incoming picture.",
    },
    TransitionInfo {
        id: "wipe-right",
        name: "Wipe Right",
        category: "Motion",
        description: "An edge sweeps rightwards, uncovering the incoming picture.",
    },
    TransitionInfo {
        id: "push",
        name: "Push",
        category: "Motion",
        description: "The incoming picture pushes the outgoing one off to the left.",
    },
    TransitionInfo {
        id: "zoom",
        name: "Zoom",
        category: "Motion",
        description: "The outgoing picture zooms away as the incoming one zooms into place.",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    /// The ids the window's media pane lists under its two transition shelves.
    fn window_ids() -> Vec<String> {
        let source = include_str!("../../puppetcat/ui/workspace/media-pane.slint");
        let from = source
            .find("property <[CatalogueItemData]> transitions-basic")
            .expect("the basic shelf");
        let to = source[from..]
            .find("property <[CatalogueItemData]> audio-voice")
            .expect("the shelf after the transitions")
            + from;
        source[from..to]
            .split("id: \"")
            .skip(1)
            .filter_map(|rest| rest.split('"').next())
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn the_window_lists_exactly_these_transitions() {
        let listed: Vec<&str> = TRANSITIONS.iter().map(|t| t.id).collect();
        assert_eq!(window_ids(), listed);
    }

    #[test]
    fn every_transition_enabled_in_the_window_is_one_the_engine_resolves() {
        // The engine's motion kinds are the other half of this list.
        for info in TRANSITIONS.iter().filter(|t| t.category == "Motion") {
            assert!(
                puppetcat_core::timeline::MotionKind::parse(info.id).is_some(),
                "{} has no motion",
                info.id
            );
        }
    }
}
