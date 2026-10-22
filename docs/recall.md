# The recall on the `neurostrata_append_log` response

A turn that writes to the Episodic Buffer gets an answer back. Until the recall, that
answer was only about the write: the file, its size, whether it rolled, the tags. The
memory the turn had just touched was invisible to the turn that touched it. A newly
written row stamps `access_count: 0` (`src/server.rs:1131`), the snapshot sorts by
`access_count` and cuts to five (`src/server.rs:1476-1491`), and from the snapshot's
point of view a memory nobody had ever read was indistinguishable from a memory that did
not matter.

The recall closes that hole by riding along on the write. The response of
`neurostrata_append_log` now carries the memories the turn's own tags matched, as
pointers, next to the answer the append always gave. The turn that touched a memory can
now see what it touched.

## What the response says

`recalled` is always there. Every response in the append's response family carries it,
the successful one and the failed one alike (`src/buffer.rs:199`, `:316`) — it is an
array, and an empty array is not by itself a verdict. An empty array *with* a
`recalled_from` means a search ran and matched nothing. An empty array *without* one
means no search produced rows: either no search ran (an untagged turn) or the search
failed — the array alone does not separate those two. The caller never has to
distinguish an absent key from an honest zero. (The refusals that precede the append —
a missing argument, an entry the secret scan rejects, a disabled buffer — are plain
strings and are not part of that family (`src/buffer.rs:138-150`, `:162-169`).)

Each entry is a pointer with five keys: `id`, `memory_type`, `domain`, `score` and
`snippet` (`src/buffer.rs:278-284`). The snippet is the first 120 characters of the
memory's content (`src/buffer.rs:277`) — enough to recognize the row, not enough to be a
second copy of it. There are at most three entries (`src/buffer.rs:255`, `:274`).

What the recall searches is the turn's own tags. The tags are normalized to domain slugs
(`src/buffer.rs:172-176`); the slugs joined together are the query, embedded once
(`src/buffer.rs:234-235`), and the same slugs are the key set the domain filter compares
against (`src/buffer.rs:251-259`). An untagged turn, or a turn whose tags normalize to
no slug at all, has no query to embed and no key set to filter by, so it recalls nothing.

`recalled_from` is a different claim: it says *how* the rows were chosen, and it is
present exactly when a search ran (`src/buffer.rs:303`, `:318-320`). `"domain"` means the
domain filter narrowed the candidates; `"vector"` means the filter was skipped and the
unfiltered candidates came back (`src/buffer.rs:288-291`).

The mode is absent in three cases, and each of them is an absence of work rather than an
absence of truth. It is absent on an untagged turn, or on a turn whose tags normalize to
no domain slug (`src/buffer.rs:206-207`): no query was built and no search ran, so there
is no mode to report. It is absent when the search failed — the embedding failed, the
namespace could not be initialized, or the search itself failed
(`src/buffer.rs:236-248`, `:262-267`) — because no rows came back to attribute. And it is
absent on the append-failure path, where the response says `appended: false`, carries an
`error`, and holds `recalled: []` with no mode (`src/buffer.rs:190-202`): the write
failed, so no search ran.

Reporting `"vector"` in any of those cases would be the misreport the mode exists to
prevent — it would claim a vector search that never happened. Silence is the honest
answer, and it is a different answer from each of the two the mode can carry. A caller
that reads `recalled_from` can conclude that a search ran and that this is how it chose.

## The filter is soft, and the mode says which happened

A `domain` that matches nothing must not blind the recall. The filter is soft: when it
would empty the candidate set it is skipped, and the unfiltered candidates are returned
instead. A wrong domain narrowing to nothing is a fact about the domain, not a reason to
return nothing at all.

The mode is what keeps that substitution visible. `recalled_from: "domain"` says the
filter chose these rows. `recalled_from: "vector"` alongside a non-empty `recalled` says
the filter you asked for found no row, so these are unfiltered rows — a different answer,
and one the caller needs in order to read the result correctly.

## Only direct matches are recalled

The store returns primaries and bounded graph expansions together: one-hop neighbours,
episode siblings, governing two-hop rows. An expansion row is context *for* a match, not
a match — it was reached because something else was reached, and it carries no claim of
its own about the query. The recall keeps only the rows whose evidence carries a direct
match (`src/buffer.rs:270-274`), using the shared predicate
`crate::traits::is_direct_match` (`src/traits.rs:100-105`) — the same test the search
handler uses to decide what counts as a direct hit, so the two cannot drift. Without that
test, a namespace with fewer than three primaries would recall the graph neighbours it
never matched.

## The recall never promotes anything

A recalled memory is not a promoted one. The recall does not move `access_count`: the
increment is not on this path at all, and a test holds it off
(`src/buffer.rs:664-668`). Promotion stays the explicit act it always was — a
`neurostrata_get_memory` call, which is the thing that bumps the counter
(`src/server.rs:2706`).

The entries are pointers, which is the same statement from the other side: an id, a type,
a domain, a score and a 120-character snippet. Reading the memory behind a pointer is a
separate, deliberate call. A recall can point at a memory all day without that memory
ever being read, and nothing in the recall's own answer pretends otherwise.

## The recall is additive

The recall rides next to the append's own result, and the append's result is unchanged:
`file`, `size_bytes`, `rolled` and `tags` are reported exactly as before
(`src/buffer.rs:304-320`). Nothing about a recall can suppress anything — no row is
removed from the response, no tag is dropped, no write is downgraded because the recall
found nothing. A turn that recalls three memories and a turn that recalls none leave the
write's own answer identical. The recall can only add to what the caller is told.

## The same thing on the search side

`neurostrata_search_memory` takes an optional `domain` — a slug or a list of slugs,
normalized to lowercase-kebab (`src/server.rs:192`, `:2006-2022`). With no `domain` the
tool behaves exactly as it did before and prints no mode line at all
(`src/server.rs:2057`; the assertion that pins it at `src/server.rs:3165-3168`). With a
`domain`, the report's first line says which happened (`src/server.rs:2059-2078`):

```
domain filter: applied (domain)
domain filter: skipped — no row matched the domain; the results are unfiltered (vector)
domain filter: skipped — the domain did not normalize; the results are unfiltered (vector)
```

The two surfaces say the same thing in different words. The recall's `recalled_from` is
the search's mode line in one word; the search's mode line is the recall's `recalled_from`
in a sentence. One difference is worth naming. The search side has a third case the
recall does not: a domain that did not normalize at all. The search tool is handed a
domain the caller typed, so a value that normalizes to nothing has to be reported as a
skip; the recall derives its domain from tags it has already normalized, and a tag set
that normalizes to nothing means there was no domain to filter by — so that case lands
on "no search ran", and the mode is absent rather than naming a skip.

---

## A failed attempt is evidence, not a rule

A `fact` carrying `metadata.attempt` is **evidence of one episode, not a normative rule**. It is returned by a search, it is rendered with its `Attempt [FAILED in …]` line, and its truncated snippet carries the `[FAILED ATTEMPT] ` prefix. A rule is normative; a failed attempt is evidence. The attempt shape is declared at vocabulary version 6 (`fact.fields.attempt`, `src/schemas/memory-vocabulary.v3.json`); a malformed attempt warns but the fact is still written.

## The domain registry

Each namespace has one registry row, a `fact` with `metadata.domain_registry` listing the registered slugs. The first domain write seeds it from the namespace's own rows. A slug that neither the registry nor the namespace's data carries is refused with the registered list and the `register_domain: true` remedy. The registry is a cache, not the truth: it can be stale, and the reconcile repairs it. A non-string `domain` is rejected, not guessed at.

The registry row is excluded from recall and from the snapshot, so it never surfaces as a memory. An imported record's domain is unioned into the local registry after the apply succeeds, so no shared record is ever refused for a domain the local registry has not seen.
