<!--
Copy to evolution/<kebab-case-name>/README.md and fill in.
Read evolution/README.md first — especially "The one discipline that matters".
Delete these comments and any section that genuinely does not apply.
-->

# <Module title — what was done, in one line>

- **Status:** exploring <!-- module axis: exploring | published | absorbed | superseded. `accepted`/`declined` belong to a PROPOSAL, never here. -->
- **Author:** <name> <<contact>>
- **Dates:** first published YYYY-MM-DD, last updated YYYY-MM-DD
- **Scope:** <which parts of the project this touches, e.g. `tooling/nl-ingest-openapi`, `spec/expressiveness.md`>
- **Provenance:** upstream commit `<sha>`; <every external input, pinned to a version or revision>
- **Resolution:** —

## Summary

What was done and what came out of it, in a paragraph a reader can stop after. Lead with the result,
including the uncomfortable parts.

## Provenance of the inputs

<!-- A table is usually clearest. Anything a stranger would need to reproduce your numbers:
     versions, revisions, dates, commit shas, URLs. If a value drifts (a live API), say when you
     read it. A result nobody can reproduce is an anecdote. -->

## What was measured

<!-- Numbers, each with the command or procedure that produced it. No inferences in this section.
     If a number is approximate or was observed once, say so here rather than later. -->

## What is argued

<!-- Your inferences from the above, clearly separable from it. A reader must be able to reject
     everything in this section and still keep everything in the previous one. -->

## What worked well

<!-- Not flattery — the parts that materially changed how you built on the project. This is signal
     the maintainers cannot get from a defect report, and it tells the next author what to lean on. -->

## Defects reported

<!-- One per entry: the observable behaviour, a minimal reproduction, and the scope of the exposure.
     The FIX belongs in its own PR — link it here, do not carry it in the module. -->

## Proposals

<!-- Changes needing a maintainer decision. One file each under proposals/NN-short-name.md, each
     opening with its OWN status line (proposed | accepted | declined | absorbed) that moves
     independently of this module's. State the change, its blast radius, what it breaks, and the
     questions only a maintainer can answer. Include the diff inline if it is small, so the
     proposal is self-contained. -->

## Open questions

<!-- Where you got stuck, or where the next person should start. Be specific enough to be actionable
     — "retrieval is hard" helps nobody; "resolving an unnamed composite by intent needs semantic
     ranking the stdlib lexical embedder cannot provide" is a starting point. -->

## Reproducing

<!-- Commands. State any credential or network requirement, and which findings need neither. -->
