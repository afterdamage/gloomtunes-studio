//! egui views, widgets and the Gloom theme for GloomTunes Studio.
//!
//! This crate never touches audio or MIDI devices. Views take plain models and return actions;
//! `gt-app` turns those actions into device or engine calls.

#![forbid(unsafe_code)]

pub mod theme;
pub mod views;
pub mod widgets;

pub use theme::GloomTheme;
