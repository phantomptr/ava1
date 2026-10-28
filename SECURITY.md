# Security policy

AVA1 is a cryptographic protocol, so please report a suspected weakness privately rather than in
a public issue: use GitHub's private vulnerability reporting
(<https://github.com/phantomptr/ava1/security/advisories/new>).

Useful to include: what an attacker controls (network position, a paired device, a malicious
node), the version or commit, and a reproduction or test if you have one.

Supported: the latest 1.x release. The design notes in [spec/SPEC.md](spec/SPEC.md) (§4 keys and
sealing, §5 handshake and pairing) and the nonce audit in
[spec/AUDIT-nonce.md](spec/AUDIT-nonce.md) describe what the protocol is meant to guarantee.
