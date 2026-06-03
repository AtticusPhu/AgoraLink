# AgoraLink Chat Foundation

AgoraLink is a LAN-first RUDP transfer and chat project. The name uses Latin letters and is derived from the Greek word Agora, meaning a public meeting space.

This version adds the first local group-chat foundation:

- `CHAT_MESSAGE`
- `CHAT_ACK`
- SQLite local chat database
- Scrypt password-derived local storage key
- AES-256-GCM encrypted message bodies
- Plaintext metadata for contacts, groups, timestamps, and delivery state
- Local P2P group model: sender sends the same logical group message to each active member one by one
- Per-recipient delivery receipt table

## Example: start receiver with chat database

```powershell
python server.py --bind 0.0.0.0 --port 9999 --save-dir .\received --chat-db .\chat_receiver.db --chat-password "your password" --chat-local-peer-id peerB
```

## Example: send one chat message

```powershell
python client.py --server-ip 192.168.0.108 --server-port 9999 --chat-message "hello" --chat-group-id group1 --chat-sender-peer-id peerA --chat-receiver-peer-id peerB --chat-db .\chat_sender.db --chat-password "your password"
```

## Notes

- SQLite is used as the local client database.
- Message text is not stored as plaintext. It is saved in `messages.encrypted_body`.
- Metadata such as `message_id`, `group_id`, `sender_peer_id`, timestamps, and status remains plaintext.
- `CHAT_ACK` means the receiver chat layer parsed and accepted the message.
- Offline resend is not implemented in this version.
