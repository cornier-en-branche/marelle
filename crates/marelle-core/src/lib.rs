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

/// A function call requested by an assistant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    /// Provider-generated call identifier.
    pub id: String,
    /// Marelle tool name.
    pub name: String,
    /// JSON-encoded arguments.
    pub arguments: String,
}

/// One message in a conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// Who produced the message.
    pub role: Role,
    /// The message text or tool result.
    pub content: String,
    /// Name attached to a tool result, when required by the provider.
    pub name: Option<String>,
    /// Identifier of the call answered by a tool message.
    pub tool_call_id: Option<String>,
    /// Calls requested by an assistant message.
    pub tool_calls: Vec<ToolCall>,
}

impl Message {
    /// Creates a plain message.
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            name: None,
            tool_call_id: None,
            tool_calls: Vec::new(),
        }
    }

    /// Creates an assistant message containing tool calls.
    pub fn with_tool_calls(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            name: None,
            tool_call_id: None,
            tool_calls,
        }
    }

    /// Creates a result message for a particular tool call.
    pub fn tool_result(call_id: impl Into<String>, name: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            name: Some(name.into()),
            tool_call_id: Some(call_id.into()),
            tool_calls: Vec::new(),
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

    /// Removes and returns the last message.
    pub fn pop(&mut self) -> Option<Message> {
        self.messages.pop()
    }

    /// Forgets the whole conversation.
    pub fn clear(&mut self) {
        self.messages.clear();
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
    fn the_last_message_can_be_taken_back_and_the_session_cleared() {
        let mut session = Session::new();
        assert_eq!(session.pop(), None);
        session.push(Message::new(Role::User, "one"));
        session.push(Message::new(Role::Assistant, "two"));
        assert_eq!(session.pop(), Some(Message::new(Role::Assistant, "two")));
        assert_eq!(session.len(), 1);
        session.clear();
        assert!(session.is_empty());
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
