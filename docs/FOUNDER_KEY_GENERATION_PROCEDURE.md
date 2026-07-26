# Founder Ed25519 key generation procedure — 2026-06-30 incident recovery

**Purpose**: produce a fresh 32-byte Ed25519 keypair whose public half replaces the founder pubkey `eed9f8f6fa68d6272fb81229ca311bd0836e38a188d433253adb2d503564a2e3` in `master-nodes.toml`, and whose private half is the new master authority for the `Authorized::Founder` path inside rope-node.

**Audience**: the human operator. The AI agent does not, can not, and must not see the private half.

## Non-negotiable properties of the new private half

1. **Must be generated on a machine other than the Mac that hosts the project workspace.** The workspace's "always-applied" Cursor rules synced the deployer secp256k1 key to whichever exfiltration path delivered it to the attacker; the same path would compromise an Ed25519 key generated on that Mac.
2. **Must be generated on a machine other than the rescue laptop `0xCF884C81…082Eb`.** The rescue laptop signed an 8,000 FAT inbound transfer on 2026-06-30 ~09:42Z, making it operationally "warm". Any new key generated on a warm machine is subject to the same compromise vector that may already exist there.
3. **The public half is, by definition, public.** Pasting it in chat is safe; committing it to `master-nodes.toml` in the public repo is correct.
4. **The private half NEVER leaves the offline machine in cleartext**, and **NEVER appears in chat, email, cloud storage, or any networked file system in any form.**

## Decision tree

| Hardware in front of you | Path | Time | Quality |
|---|---|---|---|
| YubiKey 5 + GPG suite | A — YubiKey OpenPGP Ed25519 | ~30 min first-time | Best |
| Ledger Nano S/X/S+ + ledger-app-ssh | B — Ledger on-device | ~30 min first-time | Best |
| A spare laptop / desktop you can take offline | **C — offline machine + openssl** (recommended) | ~15 min | Very good |
| USB stick + any bootable laptop | D — Tails USB | ~45 min including download | Very good |

This document covers Path C in full. Paths A, B, D are available on request.

---

## Path C — offline machine + openssl

### Materials

- An offline-capable machine, **not** the workspace Mac and **not** the rescue laptop. macOS, Linux, or Windows are all fine.
- A USB stick (any size) for backups.
- Paper and pen for paper backups.
- ~15 minutes of attention.

### Step 1 — disconnect the machine

- WiFi off, Ethernet unplugged, Bluetooth off (paranoia, optional).
- Verify: open a browser, attempt to load any URL — should fail.
- Stays offline through Step 6.

### Step 2 — verify openssl

```bash
openssl version
```

Expected: `OpenSSL 1.1.1` or higher, or `LibreSSL 3.3.6+`. Anything older lacks Ed25519 — request Python fallback.

### Step 3 — generate the keypair

In a directory you choose (e.g. Desktop):

```bash
# 1. Generate the Ed25519 private key to a PEM file
openssl genpkey -algorithm Ed25519 -out founder_key.pem

# 2. Lock down permissions (Unix only)
chmod 600 founder_key.pem

# 3. Extract the 32-byte raw public key as 64 hex characters (lowercase, no prefix)
openssl pkey -in founder_key.pem -pubout -outform DER | tail -c 32 | xxd -p -c 64
```

The third command prints **one line of exactly 64 lowercase hex characters**. That line is your new `FOUNDER_PUB`.

If the output is empty, garbled, or longer/shorter than 64 chars, stop and report the issue.

### Step 4 — sanity-check round-trip

```bash
# Same extraction, twice — both must print identical lines
openssl pkey -in founder_key.pem -pubout -outform DER | tail -c 32 | xxd -p -c 64
openssl pkey -in founder_key.pem -pubout -outform DER | tail -c 32 | xxd -p -c 64

# Verify the PEM is a real Ed25519 private key
openssl pkey -in founder_key.pem -text -noout 2>&1 | head -3
```

The two extraction lines must be byte-identical. The third command must show `ED25519 Private-Key:` (or equivalent) in the first line.

### Step 5 — backup the private key

Three layers, all required for a master authority:

**Layer 1 — encrypted USB**

```bash
openssl enc -aes-256-cbc -pbkdf2 -iter 600000 -salt \
    -in founder_key.pem -out founder_key.pem.enc
```

Choose a passphrase you can recite from memory (six unrelated common words works; a single random string of 12+ chars does too). Copy `founder_key.pem.enc` to a USB stick. Store in a physically secure location.

**Layer 2 — second USB, different location**

Copy the same `founder_key.pem.enc` to a second USB stick. Store at a different physical location (different building, safe deposit box, trusted family member, etc.). Survives single-location loss (fire, theft).

**Layer 3 — paper printout**

```bash
cat founder_key.pem
```

The PEM is ~120 chars across 4 lines. Print on paper. Store with the encrypted USBs (or at a third location).

### Step 6 — shred the cleartext

```bash
# macOS / Linux
shred -u founder_key.pem 2>/dev/null || rm -P founder_key.pem
```

Cleartext is now gone from the offline machine. Encrypted backups remain on the USB sticks and on paper.

### Step 7 — capture the 64-char pubkey for transit

```bash
# Re-extract once more if needed
openssl pkey -in founder_key.pem.enc -passin pass:<your_passphrase> -pubout -outform DER | tail -c 32 | xxd -p -c 64
```

Or if you kept the cleartext key file:

```bash
openssl pkey -in founder_key.pem -pubout -outform DER | tail -c 32 | xxd -p -c 64
```

Wait, you already shredded the cleartext at Step 6. So use the encrypted form with `-passin`, or extract before Step 6.

Practical sequence: between Steps 4 and 5, copy the pubkey into a small file `founder_pub.txt` (just the 64 chars). After Step 6, the only remaining cleartext is `founder_pub.txt`, which is safe to keep on the offline machine or move to the workspace Mac via USB.

### Step 8 — paste in chat

Open this Mac. Paste the 64-character lowercase hex string in chat. Nothing else (no `0x` prefix, no surrounding text). For extra safety against transcription errors, paste twice on two separate lines and the AI agent will verify they match.

---

## What the agent does the moment you paste

1. Validate the string is exactly 64 lowercase hex chars.
2. Compute `keccak256(pubkey_bytes)` for the on-chain `recordUntie(executiveAuthorityHash=...)` commitment.
3. Run `patches/founder-key-rotation/rotate-founder-key.sh <FOUNDER_PUB>` to patch `master-nodes.toml`, rsync to all 4 master nodes, rolling restart `datachain-rope.service`.
4. Smoke-test old key → `Denied`, new key → `Founder`.
5. Phase A complete; Phase B/C/D/E/F resume per `RECOVERY_EXECUTION_CHECKLIST_2026-06-30.md`.

---

## When you'll use the private key today

Exactly once, off-line, around T+1:30 (~30 min after pasting the pubkey). You'll receive a small `message.bin` file containing the recovery declaration bytes. Sign with:

```bash
# On the offline machine, decrypt to a temp file first
openssl enc -d -aes-256-cbc -pbkdf2 -iter 600000 \
    -in founder_key.pem.enc -out /tmp/founder_key.pem
chmod 600 /tmp/founder_key.pem

# Sign
openssl pkeyutl -sign -inkey /tmp/founder_key.pem -rawin -in message.bin -out signature.bin

# Shred immediately
shred -u /tmp/founder_key.pem 2>/dev/null || rm -P /tmp/founder_key.pem
```

Carry `signature.bin` back via USB. That's the only use today. After T+3:30, the key returns to its encrypted backups until the next Tier-S exercise (target: never).

---

## Things that are not OK

- ❌ Generating on the workspace Mac.
- ❌ Generating on the rescue laptop.
- ❌ Emailing the PEM to yourself.
- ❌ Uploading to iCloud / Google Drive / Dropbox / OneDrive / GitHub / GitLab / any cloud.
- ❌ Pasting the PEM into any chat, including with the AI agent.
- ❌ Storing the passphrase together with the encrypted PEM.
- ❌ Running `cat founder_key.pem` while screen-sharing or with another person looking.
- ❌ Reconnecting the offline machine to the network before Step 6 completes.
- ❌ Reusing this key for any other purpose (SSH, GPG, code signing, etc.).

## Snags and fallbacks

| Symptom | Resolution |
|---|---|
| `openssl version` shows < 1.1.1 or LibreSSL < 3.3.6 | Switch to the Python pure-stdlib Ed25519 fallback (request from agent) |
| `openssl genpkey -algorithm Ed25519` errors with "algorithm not supported" | Same as above |
| Pubkey extraction line is empty or wrong length | Re-run from Step 3; if persistent, switch to Python fallback |
| Lost passphrase for `founder_key.pem.enc` | If paper backup exists, use it. If not, the key is gone — generate a fresh one and start over BEFORE pasting the lost pubkey to the agent |
| Two extraction lines (Step 4 round-trip) differ | PEM file is corrupted — regenerate from Step 3 |
| Want to use a YubiKey 5 instead | Request Path A walkthrough |
| Want to use a Ledger instead | Request Path B walkthrough |
| Want to use Tails USB instead | Request Path D walkthrough |

— Datachain Foundation, 2026-06-30
