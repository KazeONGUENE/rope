#!/usr/bin/env python3
"""Patch cause-c1f30613 campaign fields in projects.jsonl (idempotent)."""
from __future__ import annotations

import json
import sys
from pathlib import Path

PROJECT_ID = "cause-c1f30613"

ABOUT = """Every token project accumulates a remainder. Tokens sitting at a deployer address after a launch, a migration, a sale that closed. In this industry that remainder almost always ends up in one of three places: absorbed quietly into a treasury, sold into the market, or parked behind a phrase like \"reserved for future ecosystem use,\" which commits to nothing and means nothing.

We are proposing something else. The remaining legacy DC allocation should be directed by the people who hold the token, not by the people who happen to hold the keys.

This vote decides whether the community, rather than the Foundation, directs the remaining legacy DC allocation. Nominations are open now: any participant can propose a non-governmental organisation as a candidate to receive that residual reserve. Vote For, and those nominations feed the selection process. A jury is drawn from the governance pool by verifiable random selection. The winning organisation registers a treasury it controls, signs to the milestones it proposed, and every disbursement afterwards is a public transaction anyone can audit. Vote Against, and the allocation stays where it is.

Voting costs you nothing. Your FAT is locked for the length of the window and released back to your wallet when it closes. That is the Return disposition, chosen deliberately here: no one should have to pay for the right to decide where value goes.

The only way this fails is silence. Approval needs more than half the weight cast, and at least 1,000,000 FAT of total participation. A proposal can be supported by everyone who reads it and still die below quorum. If you hold DC on Ethereum, DC on XDC, or FAT or WFAT on Rope, your balance counts toward that threshold. Cast the ballot."""

USE_CASES = """What a For vote unlocks:

- A residual token allocation is converted into funded, milestone-gated charitable work instead of quiet retention.
- Smaller holders who could never write a large cheque get direct authority over where meaningful value lands.
- Datachain Rope's governance stack gets its first live, consequential rehearsal on a subject where failure is recoverable and success is visible.
- Any organisation seeking funding gains a public, verifiable route to it that does not depend on knowing the right people."""

FUNDING = """Exact residual under confirmation; the figure will be published before the selection vote opens.

Directed entirely to the non-governmental organisation selected by the subsequent community vote. No portion is retained by the Foundation. Release is milestone-gated against the terms the winning organisation sets out in its own submission, and every transfer is a public transaction on Datachain Rope."""

TECH_DETAIL = (
    "VoteEscrow governance contract on Datachain Rope (chain ID 271828), Timelock-owned. "
    "Return disposition. EIP-191 signed ballots. Cross-chain weight attestation covering "
    "Ethereum, XDC, and Rope. BLAKE3 deterministic jury selection with published seed. "
    "Governance events anchored as knots on the Quipu ledger and readable over public RPC and DCScan."
)

ARCHITECTURE = (
    "Ballots are cast against a single audited escrow contract whose cost model is fixed on-chain "
    "before the window opens, so no term can change under a voter mid-vote. Voting weight is "
    "computed by summing your holdings across every form of the asset and attested cryptographically, "
    "which is what lets holders who have not yet migrated participate. Locked balances are released "
    "automatically at finalisation. If this proposal passes, the beneficiary treasury is bound by "
    "the winner's own signature, and disbursement runs through a Timelock-owned path rather than a "
    "manual transfer, so the entire route from nomination to payment is reconstructible by an "
    "outside party with no access to our systems."
)

PATCH = {
    "name": "Community NGO Campaign By Datachain Foundation",
    "tagline": "The remainder is not ours to keep.",
    "submitterName": "Datachain Rope Team",
    "organizationName": "Datachain Foundation",
    "mission": (
        "Decide whether the community, rather than the Foundation, directs the "
        "remaining legacy DC allocation to a non-governmental organisation chosen by "
        "open nomination and verifiable vote."
    ),
    "description": ABOUT,
    "features": [
        "**Community-directed, not foundation-directed.** The Foundation holds these tokens and is asking to be told what to do with them.",
        "**Free to participate.** Return disposition. Locked FAT is released to your wallet at close.",
        "**Open nomination now.** Propose an NGO on this page while the vote is open. If For wins, the shortlist is built from these community nominations - not written by the Foundation.",
        "**Randomly drawn jury.** Cohorts are selected deterministically from a published seed, so the panel is independently re-derivable and not hand-picked.",
        "**Cross-chain franchise.** Legacy DC on Ethereum, legacy DC on XDC, native FAT and WFAT on Rope all count. Not having migrated yet does not cost you your vote.",
        "**Auditable to the last transfer.** Nomination, ballot, contract terms, and every payment are anchored on the ledger and visible on DCScan.",
    ],
    "useCases": USE_CASES,
    "milestones": [
        {
            "title": "Vote closes",
            "description": "Result and full tally anchored on the governance ledger.",
        },
        {
            "title": "Nomination (open now)",
            "description": "Members and organisations submit NGO candidates on this campaign page: legal entity, mission, requested amount, milestones, references.",
        },
        {
            "title": "Legitimacy review",
            "description": "A moderation gate confirms each applicant is a real, verifiable organisation. This is a check, not a vote, and it cannot pick a winner.",
        },
        {
            "title": "Selection vote",
            "description": "Jury drawn, ballot opened, weight attested across all four token forms.",
        },
        {
            "title": "Contractualisation",
            "description": "Winner registers a treasury by signature, and the milestones from its own submission become its binding terms.",
        },
        {
            "title": "Disbursement",
            "description": "Funds released against those milestones, every transfer a public transaction.",
        },
    ],
    "reviewOutcome": (
        "Submitted for community vote. This proposal establishes community authority over the residual legacy DC allocation. "
        "NGO nominations are open on this page; no beneficiary has been selected yet."
    ),
    "nominationsOpen": True,
    "techStack": [
        "VoteEscrow",
        "Timelock-owned",
        "Return disposition",
        "EIP-191 ballots",
        "Cross-chain weight",
        "BLAKE3 jury",
        "Quipu ledger",
    ],
    "techStackDetail": TECH_DETAIL,
    "architectureDescription": ARCHITECTURE,
    "fundingBreakdown": FUNDING,
    "fundingAskLabel": "Residual under confirmation",
    "fundingCurrency": "DC",
    "fundingRequested": 0,
    "heroImage": "/assets/causes/cause-c1f30613-hero.jpg",
    "voteIsMeta": True,
    "disposition": "return",
    "documents": [],
    "media": [],
    "campaignLinks": [
        {"label": "Community Vote", "url": "https://dcscan.io/vote"},
        {"label": "Create a Rope wallet", "url": "https://dcscan.io/create-wallet"},
        {"label": "Supply reconciliation", "url": "https://dcscan.io/supply"},
        {"label": "Governance API", "url": "https://dcscan.io/apis"},
    ],
    "websiteUrl": "https://dcscan.io/vote",
    "demoUrl": "https://dcscan.io/create-wallet",
    "whitepaperUrl": "https://dcscan.io/supply",
    "documentationUrl": "https://dcscan.io/apis",
    "campaignCopySchema": "datachain-rope/campaign-copy",
    "campaignCopyVersion": 1,
    "campaignCopyStatus": "draft_pending_variables",
    "publicationBlockers": [
        {
            "key": "SIGNING_CAPABILITY_CONFIRMED",
            "blocks_publication": True,
            "note": (
                "The known-compromised deployer address is refused by every admin script. "
                "If the residual sits there, an approved vote produces a mandate that "
                "cannot be executed. Confirm before treating this campaign as executable."
            ),
        }
    ],
}


def assert_copy_policy(text: str) -> None:
    if "\u2014" in text or "\u2013" in text:
        raise SystemExit("em/en dash found in copy")
    for needle in (
        "Voting costs you nothing.",
        "The only way this fails is silence.",
        "The remaining legacy DC allocation should be directed by the people who hold the token, not by the people who happen to hold the keys.",
    ):
        if needle not in text:
            raise SystemExit(f"missing protected string: {needle}")


def main() -> int:
    path = Path(sys.argv[1] if len(sys.argv) > 1 else "/opt/datachain-rope/projects.jsonl")
    assert_copy_policy(ABOUT)
    if not path.exists():
        raise SystemExit(f"missing {path}")
    lines = path.read_text(encoding="utf-8").splitlines()
    out = []
    found = False
    for line in lines:
        if not line.strip():
            continue
        obj = json.loads(line)
        if obj.get("id") == PROJECT_ID:
            found = True
            if obj.get("disposition") not in (None, "return"):
                raise SystemExit(f"halt: disposition is {obj.get('disposition')!r}, expected return")
            quorum = obj.get("requiredQuorumFat", 1_000_000)
            if float(quorum) != 1_000_000.0:
                raise SystemExit(f"halt: requiredQuorumFat is {quorum}, expected 1000000")
            obj.update(PATCH)
            # Keep empty documents/media if already populated with real attaches.
            if isinstance(obj.get("documents"), list) and obj["documents"] and PATCH["documents"] == []:
                pass  # PATCH already set documents to [] — restore prior if non-empty?
            out.append(json.dumps(obj, ensure_ascii=False, separators=(",", ":")))
        else:
            out.append(json.dumps(obj, ensure_ascii=False, separators=(",", ":")))
    if not found:
        raise SystemExit(f"project {PROJECT_ID} not found in {path}")
    # Preserve existing non-empty documents/media from a re-read merge
    # Re-apply smarter: if previous had docs, don't wipe — handled below
    tmp = path.with_suffix(".jsonl.tmp")
    # Second pass: reload original for media preservation
    originals = {json.loads(l).get("id"): json.loads(l) for l in lines if l.strip()}
    prev = originals.get(PROJECT_ID, {})
    rebuilt = []
    for line in out:
        obj = json.loads(line)
        if obj.get("id") == PROJECT_ID:
            prev_docs = prev.get("documents") if isinstance(prev.get("documents"), list) else []
            prev_media = prev.get("media") if isinstance(prev.get("media"), list) else []
            if prev_docs:
                obj["documents"] = prev_docs
            if prev_media:
                obj["media"] = prev_media
            rebuilt.append(json.dumps(obj, ensure_ascii=False, separators=(",", ":")))
        else:
            rebuilt.append(line)
    tmp.write_text("\n".join(rebuilt) + "\n", encoding="utf-8")
    tmp.replace(path)
    print(f"patched {PROJECT_ID} in {path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
