//! vu — sound meter library: analysis, sources, recording, and the terminal
//! and window UIs.

#![allow(clippy::needless_range_loop)]

pub mod analyzer;
pub mod app;
pub mod cli;
pub mod dsp;
pub mod generator;
#[cfg(feature = "gui")]
pub mod gui;
pub mod history;
pub mod pump;
pub mod recorder;
pub mod report;
pub mod source;
pub mod ui;
