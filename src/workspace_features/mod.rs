//! `workspace` — Feature modules for stack_ide workspace management.
//!
//! | Module         | Feature |
//! |----------------|---------|
//! | `session`      | 1 — Session Restore |
//! | `roots`        | 2 — Multiple Workspace Roots |
//! | `editorconfig` | 3 — .editorconfig Support |
//! | `templates`    | 4 — Project Templates |
//! | `tasks`        | 5 — Task Runner |
//! | `trust`        | 6 — Workspace Trust |
//! | `recent`       | 7 — Recently Opened Projects |
//! | `exclude`      | 8 — Exclude Patterns |

pub mod session;
pub mod roots;
pub mod editorconfig;
pub mod templates;
pub mod tasks;
pub mod trust;
pub mod recent;
pub mod exclude;
