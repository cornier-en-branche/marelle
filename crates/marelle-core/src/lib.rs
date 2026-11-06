// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Marelle contributors

//! Core data types shared by every Marelle crate.

/// Who produced a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Instructions that frame the whole conversation.
    System,
    /// The person at the keyboard.
    User,
    /// The model.
    Assistant,
    /// The result of a tool call.
    Tool,
}

/// One message in a conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// Who produced the message.
    pub role: Role,
    /// The message text.
    pub content: String,
}

impl Message {
    /// Creates a message.
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
        }
    }
}

/// An ordered conversation.
#[derive(Debug, Clone, Default)]
pub struct Session {
    messages: Vec<Message>,
}

impl Session {
    /// Creates an empty session.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a message to the end of the conversation.
    pub fn push(&mut self, message: Message) {
        self.messages.push(message);
    }

    /// The messages so far, oldest first.
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// How many messages the session holds.
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    /// Whether the session holds no messages.
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_session_is_empty() {
        let session = Session::new();
        assert!(session.is_empty());
        assert_eq!(session.len(), 0);
    }

    #[test]
    fn messages_keep_their_order() {
        let mut session = Session::new();
        session.push(Message::new(Role::User, "hello"));
        session.push(Message::new(Role::Assistant, "hi"));

        let roles: Vec<Role> = session.messages().iter().map(|m| m.role).collect();
        assert_eq!(roles, [Role::User, Role::Assistant]);
        assert_eq!(session.messages()[1].content, "hi");
    }
}
