# AgoraLink chat store baseline

This build adds the first usable local group-chat layer on top of the existing RUDP transfer stack.

## Implemented

- `CHAT_MESSAGE` and `CHAT_ACK` protocol frames.
- Local SQLite chat database.
- Message body encryption with a password-derived Scrypt storage key.
- Plaintext metadata for contacts, groups, timestamps, states and receipts.
- `chat_store.py` business layer:
  - `create_group()`
  - `add_group_member()`
  - `remove_group_member()`
  - `leave_group()`
  - `send_group_message()`
  - `save_incoming_chat_message()`
  - `mark_chat_delivered()`
- GUI chat tab:
  - unlock chat DB with password
  - create/save group
  - add/update/remove group members
  - leave group
  - show members and messages
  - send one group message to active members one by one
  - show per-member delivery summary

## Group-chat model

There is no server and no offline replay. Group messages are sent by the sender to every currently active group member over separate RUDP secure sessions. Each recipient returns `CHAT_ACK`. The local SQLite database records one receipt per recipient.

A member that leaves or is removed is marked non-active in `group_members`; new group messages are no longer sent to that member.

## Local storage

Message metadata remains queryable in SQLite. Message bodies are stored only in encrypted form using AES-256-GCM. The storage key is derived from the user's chat password with Scrypt.
