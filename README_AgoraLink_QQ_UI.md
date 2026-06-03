# AgoraLink QQ-style chat UI preview

This build changes the application direction from a file-transfer utility into a chat-first LAN messenger.

Implemented in this preview:

- Startup unlock dialog with password, confirmation field and nickname.
- Skip unlock mode. If skipped, the app remains in basic receiver/file-transfer mode.
- QQ-style chat main page: left conversation/device list, center message area and input, right member/device detail panel.
- Recent/group/device sections.
- One-to-one chat through trusted contacts.
- Group chat through active group members.
- Contact request protocol: CONTACT_REQUEST / CONTACT_RESPONSE.
- Receiver-side contact request popup with Allow / Reject.
- Contact list stores nickname/remark, endpoint and trust state.
- Create group automatically adds the local peer as owner/active member.
- Group member add/remove/leave operations, with confirmation for remove/leave.
- Send File button in chat context. It uses the original file transfer flow and does not create file message cards yet.
- Settings/debug popup with firewall helper and theme selector.
- Receiver auto-start after chat unlock, with online/offline toggle.

Current limitations:

- The QQ-style UI is a functional first pass, not a polished final chat UI.
- File sends are not yet represented as message cards.
- Contact request acceptance is stored on the receiver side. Sender-side contact creation should be refined later.
- Theme switching is immediate for the main window background but not all existing child widgets are restyled yet.
- The old send/receive pages are still present internally for skip/debug compatibility.
