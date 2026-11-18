# W13 content acceptance: SimpleLogin alias lifecycle (#1748)

This is the pre-run expected set for the content-acceptance pilot of SPEC-15
slice 8 (AC-0670, ADR-0064 decision 13). [`expected.json`](expected.json)
was written by hand from the SimpleLogin source at
`4f212552d4f8ba31e2ef8f4a8e52a642ee0f9798`, the commit pinned in
`bench/repos.toml`, before any drafting run and without reading Cartograph
output for that repository. Keep this directory outside every model input.
Recovery and drafting never read it.

## Status

The expected set and the scoring protocol are committed. No drafting run has
been scored. AC-0670 is met only when a `scores.json` and its table are
committed here, whatever the result.

## Capability set

Alias lifecycle over the API, five capabilities: create a random alias,
create a custom alias (v2), enable or disable an alias, delete an alias and
update an alias. The handlers are in `app/api/views/new_random_alias.py`,
`app/api/views/new_custom_alias.py` and `app/api/views/alias.py`.
`new_custom_alias_v3`, contacts, activities and the alias lists are outside
the set.

The pilot scores three D1 sections for that set: 1.2 Actors and roles, 1.3
Capabilities and 1.5 Business rules. Each expected item is one statement
with the source lines it rests on. There are 26: 4 for 1.2, 6 for 1.3 and 16
for 1.5.

## Run

1. Ingest SimpleLogin at the commit above, as the showcase does
   (`docs/showcases/simplelogin.md`).
2. For each of the three sections, start a drafting run from the Workbench
   ("Draft this section", whole system) and let it finish.
3. A person accepts, rejects or returns each draft in the Workbench.
4. Export in best-effort mode. The scored text is the drafted claims that
   `product-specification.md` prints for 1.2, 1.3 and 1.5.

Record the graph content hash, the curated revision, the role definition
fingerprint and the model in `scores.json`.

## Scoring

The reviewer is someone who did not produce or accept the drafts. For each
printed claim the reviewer opens every citation in `fact-index.md` and the
source it names, and records one verdict:

- **supported**: the cited sources state what the claim states. A claim that
  is true but whose citations do not show it is unsupported.
- **unsupported**: anything else, including a claim about something outside
  the capability set that its citations do not show.

The reviewer also lists the expected items a supported claim states. An
expected item no supported claim states is **missing**. A claim may state an
item only in part; it then does not cover the item, and the note says why.

`python3 -I score.py scores.json` checks the file and prints the table that
is committed beside it.

| Measure | Definition | Proposed pass |
|---|---|---|
| Support | supported claims / printed claims | 100 % |
| Coverage of 1.3 | expected capabilities stated by a supported claim | 6 / 6 |
| Coverage of 1.2 and 1.5 | expected items stated by a supported claim | at least 14 / 20 |

The pass values are a proposal until they are frozen with a one-line go.
Agreement from a reviewing agent does not freeze them. The result is
committed whether or not it passes.

## Limits

- The export compiles the base graph. A claim that can only cite a fact in
  the knowledge overlay is not printed (SPEC-15 open items), so it is
  missing here, and the note should say so.
- Membership of a citation is checked by the host. Whether the cited facts
  support the statement is what this review establishes.
