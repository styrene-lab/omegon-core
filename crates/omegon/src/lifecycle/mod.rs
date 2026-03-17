//! Lifecycle Engine — design, specification, and decomposition as cognitive modes.
//!
//! The lifecycle is not a feature crate. It's how the agent loop thinks about
//! structured work. Phase detection, ambient capture, design state management,
//! spec validation, and autonomous decomposition all live here.

pub mod capture;
// pub mod design;    // TODO: design node state machine + sqlite I/O
// pub mod spec;      // TODO: spec engine (parse, validate, compare)
// pub mod decompose; // TODO: decomposition engine (assess, fork, harvest, merge)
// pub mod store;     // TODO: lifecycle.db sqlite schema + queries
