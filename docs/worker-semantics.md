# Worker Semantics

## Index

Initialization canonicalizes the vault root. Traversal uses sorted `read_dir` recursion, does not follow symlinks, skips non-UTF-8 entries, and excludes `.git`, `.obsidian`, and `.trash`. `.obsidian/types.json` is read separately. Supported files are Markdown, Base, Canvas, common image/audio/video formats, and PDF. Attachment bodies are never read.

Markdown frontmatter is optional. Invalid note frontmatter removes only that note's properties; it does not fail the scan. Tags combine frontmatter tags and prose tags with deduplication. Wiki links are extracted from prose, excluding code and HTML. Backlinks are built after every candidate record exists. Link resolution prefers a source-relative target and then a naturally sorted basename match.

Overlays replace disk text and can create supported overlay-only records. Their creation timestamp survives updates. A complete candidate index, backlinks included, is published in one transaction. Failed scans preserve records, generation, overlays, and cached query rows. Successful publication increments generation and clears every result cache entry.

Non-UTF-8 paths are omitted. `inspect` reports their total and a bounded redacted sample; raw invalid filename bytes never enter JSON.

## Sources And Watching

File Base sources are lexical vault-relative paths. Existing disk sources are canonicalized after symlink resolution and must remain under the canonical root. Overlay sources use the same lexical boundary.

One recursive notify watcher treats events as invalidation hints. `.git`, `.trash`, and ordinary `.obsidian` events are ignored, while `.obsidian/types.json` is retained. Relevant events are coalesced for 100 ms into one full transactional rescan. Watch errors and failed rescans never make the published index unavailable.

## YAML And Expressions

YAML must contain one top-level mapping. Aliases, anchors, explicit tags, merge keys, duplicate keys, complex keys, multiple documents, non-finite numbers, unsafe property names, and nesting over the configured limit are rejected before evaluator values are constructed.

Expressions support strings, decimal numbers, booleans, null, identifiers, member/index access, calls, unary `!`, `not`, and `-`, and binary `* / + - > >= < <= == != && and || or`. Binary operators are left associative with conventional precedence. There is no JavaScript fallback or dynamic evaluation. Source bytes, AST nodes, and AST depth are bounded.

Visible roots are `file`/`note`, `this`, `property`, `formula`, `value`, and `date`. Supported file members are `file`, `name`, `path`, `backlinks`, `ctime`, `mtime`, `hasTag`, `inFolder`, and `asLink`. Links support `asFile`; dates support `date`; values support `isEmpty`; lists support `filter`, `map`, `sort`, and `slice`.

Binary logical operands are evaluated eagerly. YAML filter groups short-circuit. Formula values are cached per record, dependencies are lazy, and cycles/depth overflow are errors. One step and monotonic wall-clock budget covers all records, filters, formulas, sort keys, list callbacks, and rendered columns in a query.

## Values, Dates, And Sorting

Empty values, null, empty strings, and empty lists are empty for `isEmpty`. Hierarchical tags imply their parents. Date subtraction yields a duration; duration strings accept day, week, month, and year units with the fixed millisecond scales.

Dates accept exactly `YYYY-MM-DD` or RFC 3339 timestamps with explicit `Z` or numeric offset. Date-only values mean midnight UTC. Rendered date cells use the UTC calendar date. An invalid typed date property remains its original scalar; `date(invalid)` returns the empty evaluator value.

Natural ordering is locale-independent. ASCII digit runs compare by magnitude without integer conversion, then leading-zero count and bytes. Non-digit text compares case-sensitively by Unicode scalar value. Normalized path bytes provide the final path order. This comparator is used for view sort values, path ties, list sorting, and link fallback selection.

## Results And Limits

View and global filters run before sort and view limits. `matched_count` is pre-limit; `view_count` is post-limit. Result IDs are `r<generation>-<process sequence>` and are never reused during a process. Fetch returns complete rows, not only preview rows.

The result-byte limit is measured against compact JSON for both the query response and complete `{result_id,rows}` payload before rows are cached. Text cells strip C0/C1 controls except tab, LF, and CR. The native worker deliberately has no process-wide memory cap; source, expression, AST, YAML, formula, evaluation, row, and result-byte limits remain enforced.
