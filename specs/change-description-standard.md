# Change Description Standard

This standard applies to design documents, technical explanations and change
descriptions. Commit-specific requirements remain in the
[commit message standard](commit-message-standard.md); documentation placement
and bilingual rules remain in the
[documentation standard](documentation-standard.md).

## Why this standard exists

A reader who did not attend the discussion should understand what changes,
which concrete object changes, and why the change matters. The text must not
depend on chat history, an unexplained comparison or familiarity with a private
prototype. Implementation details support that explanation rather than replace
it.

User examples express the problem being clarified. They are not automatically
a mandatory template for every later document. Do not turn a local discussion
about handoff, ownership or a particular command into a universal chapter list.

## Writing concrete explanations

- Name the relevant objects, facts and actions. Explain unfamiliar background
  when the reader needs it, using a short concrete example when helpful.
- Describe the main change and its user or maintainer effect before the
  supporting implementation details. Explain the reason, meaningful trade-offs
  and applicable boundary without listing every changed line.
- When describing current behavior, identify the repository and named ref or
  commit being examined, and the relevant command, crate or path. Words such as
  "upstream", "current" or "the prototype" are not enough to identify a baseline.
  Distinguish existing behavior, the proposed change and remaining work.
- A Before/After comparison is useful when the contrast is hard to follow, not
  a mandatory section in every document. Parallel functions are usually clearer
  as parallel lists than as a fabricated sequence or dependency chain.
- Use complete sentences and clear references. Name a component when its owner
  or action would otherwise be ambiguous; do not mechanically repeat the same
  subject in every sentence or force every document into one opening formula.

Concrete names are useful because they remove ambiguity. For example,
"`aw-service` calls `aw-host` to execute a Hook" identifies both the actor and
the library boundary. It does not imply that all explanations must discuss
runtime boundaries or crate ownership.
