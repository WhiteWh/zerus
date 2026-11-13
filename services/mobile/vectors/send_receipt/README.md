# Phone send correlation and settled retention

`intents.json` specifies API v1's exact original intent and native digest bytes.
`compaction.json` independently freezes the same payloads with both native
encodings and compact, payload-independent correlation metadata. Android and
ZerusKit consume the identical mirrored `send-correlation.json` test resource.
These are synthetic fixtures, not captured user state.

After a terminal outcome, the phone can replace its redundant canonical intent
with a versioned frozen header, original canonical/native digests and a binding
to the request UUID, complete target, original text hash and attachment metadata.
The original text and attachment references remain in the row. Receipt reads
and exact native echo checks temporarily reconstruct and validate the original
bytes; compaction never grants send authority or chooses current session options.
Sending and uncertain rows keep their full intent, including locally dismissed
uncertainty. Unsupported or invalid compact metadata stays pinned and cannot
enter the legacy identity-absent receipt path.

The redundant settled cache has global limits of 100 rows and 1 MiB of encoded
rows. It keeps the newest eligible rows in persisted enqueue order. A row becomes
eligible only after a fresh scope-verified exact native identity represents its
complete original text and every attachment, or after explicit review moves a
failed composition into a recovery draft in the same ordered state commit.
Text/time display deduplication, a terminal status alone, local dismissal and
legacy `reviewed` rows provide no eviction authority. Active delivery/receipt
flights and referenced presentation identities stay pinned until handoff.

Legacy state decodes without a destructive migration. Normalization runs during
background load and through the existing ordered reducers, publications and
writes. The publication and background writer each own a normalizer warmed
while loading private state. Draft-only edits reuse the outgoing array and pin
snapshot in constant time. Row-content memo entries cache eligibility and encoded
size (at most 512 entries per lane), retain no reconstructed canonical payload,
and drop stale generations. Pin derivation also reuses unchanged input collections.
Changed rows or pins still run the same global retention policy. Failed durable writes retain the previous authoritative stored value;
no attachment files are deleted by this policy. Request UUIDs, API v1, private
storage keys and encryption are unchanged.

This bounds only redundant settled data. Unresolved delivery, recoverable
failures and submitted content without a complete exact echo remain protected.
The encrypted monolithic write still scales with that protected backlog; the
separate paged encrypted payload-store task is `zerus-a4mub.47`.
