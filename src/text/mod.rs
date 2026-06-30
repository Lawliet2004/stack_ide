//! Text shaping and rendering utilities.
//!
//! Provides optional ligature-aware text rendering using `cosmic-text` for
//! editors and terminals that want to display coding ligatures (e.g. `->` as
//! `→`) when a ligature-enabled monospace font is active.
//!
//! Also provides Right-to-Left (RTL) text detection and rendering support
//! for Arabic and Hebrew characters in comments.

pub mod font_loader;
pub mod ligature;
pub mod rtl;
