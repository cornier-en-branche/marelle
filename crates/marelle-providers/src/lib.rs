// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Marelle contributors

//! Talking to model providers.
//!
//! The pieces, from the outside in:
//!
//! * [`ProviderConfig`] says where a provider lives and which wire format it
//!   speaks. It is read from settings named `providers.<id>.<field>`, and
//!   [`register_presets`] fills in the well-known providers.
//! * [`Client`] sends one [`ChatRequest`] and hands the answer to a sink piece
//!   by piece, then returns a [`ChatOutcome`].
//! * [`NetworkSettings`] holds what applies to every provider: timeouts,
//!   proxy, extra certificate authorities.
//!
//! An API key is never stored in a setting. A setting names the environment
//! variable or the file that holds it, and the key travels in a [`Secret`]
//! that cannot be printed by accident.

mod anthropic;
mod chat;
mod client;
mod config;
mod decode;
mod error;
mod openai;
mod presets;
mod secret;
mod sse;

pub use chat::{ChatOutcome, ChatRequest, FinishReason, Sink, StreamEvent, ToolDefinition, Usage};
pub use client::{Client, NetworkSettings, ProxySetting, plain_client};
pub use config::{
    AuthStyle, MaxTokensField, ProviderConfig, ProviderKind, normalize_name, provider_names,
};
pub use error::ProviderError;
pub use presets::{PRESETS, Preset, preset, register_presets};
pub use secret::Secret;
