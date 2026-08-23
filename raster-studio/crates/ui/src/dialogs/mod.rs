//! Modal and modeless dialogs.
//!
//! All of them share one layout grammar — a clear title, the content, and a
//! right-aligned action row whose primary action comes last — so the app never
//! makes the user re-learn where the confirm button is. Escape cancels, Enter
//! confirms, and every control is reachable from the keyboard.
//!
//! A dialog's *state* is a plain struct with its own validation, separate from
//! its drawing. Confirming produces a command (or a settings value); cancelling
//! produces nothing. That split is what lets the interesting parts — aspect
//! locking, anchor arithmetic, colour conversions, gradient stop ordering — be
//! tested without a window.
