# Worker Semantics

## Index

Initialization canonicalizes the vault root. Traversal uses sorted `read_dir` recursion, does not follow symlinks, skips non-UTF-8 entries, and excludes `.git`, `.obsidian`, and `.trash`. `.obsidian/types.json` is read separately. Supported files are Markdown, Base, Canvas, common image/audio/video formats, and PDF. Attachment bodies are never read.

Markdown frontmatter is optional. Invalid note frontmatter removes only that note's properties; it does not fail the scan. Tags combine frontmatter tags and prose tags with deduplication. Wiki links are extracted from prose, excluding code and HTML. Backlinks are built after every candidate record exists. Link resolution prefers a source-relative target and then a naturally sorted basename match.

Overlays replace disk text and can create supported overlay-only records. Their creation timestamp survives updates. Named ordinary-file updates reuse unchanged records and reread only the affected paths. Property types are checked on every update; changes to `.obsidian/types.json` require a full scan. Backlinks are cleared and rebuilt across the complete candidate, so adding or removing a target can redirect links from unchanged notes. A complete candidate index, backlinks included, is published in one transaction. Failed scans preserve records, generation, overlays, and cached query rows. Successful publication increments generation and clears every result cache entry.

After a failed watcher update, the next update requires a full scan so that other changes in the rejected batch are recovered. The failed update preserves the published snapshot and cached results until that scan succeeds.

Non-UTF-8 paths are omitted. `inspect` reports their total and a bounded redacted sample; raw invalid filename bytes never enter JSON.

## Sources And Watching

File Base sources are lexical vault-relative paths. Existing disk sources are canonicalized after symlink resolution and must remain under the canonical root. Overlay sources use the same lexical boundary.

One recursive notify watcher treats events as invalidation hints. `.git`, `.trash`, and ordinary `.obsidian` events are ignored, while `.obsidian/types.json` is retained. Relevant events are coalesced for 100 ms into one transactional update. Precise file creation, removal, and data/metadata changes refresh named ordinary files. Ambiguous events, renames, directories, unrepresentable paths, backend rescan requests, and batches exceeding eight paths use a full rescan. An empty path in `index_changed.paths` means the watcher cannot retain a complete named change set and requires a full scan. Named hints can also trigger full scans: symlink, excluded, and metadata-type paths fall back to the scanner, which retains its traversal rules. Watch errors and failed rescans never make the published index unavailable.

## YAML And Expressions

YAML must contain one top-level mapping. Aliases, anchors, explicit tags, merge keys, duplicate keys, complex keys, multiple documents, non-finite numbers, unsafe property names, and nesting over the configured limit are rejected before evaluator values are constructed.

Expressions support strings, decimal numbers, booleans, null, identifiers, member/index access, calls, unary `!`, `not`, and `-`, and binary `* / + - > >= < <= == != && and || or`. Binary operators are left associative with conventional precedence. There is no JavaScript fallback or dynamic evaluation. Source bytes, AST nodes, and AST depth are bounded.

Visible roots are `file`/`note`, `this`, `property`, `formula`, `value`, `date`, and `today`. Supported file members are `file`, `name`, `path`, `backlinks`, `ctime`, `mtime`, `hasTag`, `inFolder`, and `asLink`. Links support `asFile`; dates support `date`; numbers support `round` with an optional decimal precision; values support `isEmpty`; lists support `filter`, `map`, `sort`, and `slice`.

Binary logical operands and ordinary call arguments are evaluated eagerly from left to right. YAML filter groups short-circuit. The single expression passed to `filter` or `map` is evaluated once per list item; other callback arities are errors. Formula values are cached per record, dependencies are lazy, and cycles/depth overflow are errors. One step and monotonic wall-clock budget covers all records, filters, formulas, sort keys, list callbacks, and rendered columns in a query.

## Values, Dates, And Sorting

Empty values, null, empty strings, and empty lists are empty for `isEmpty`. Hierarchical tags imply their parents. Date subtraction yields a duration; duration strings accept day, week, month, and year units with the fixed millisecond scales.

Dates accept exactly `YYYY-MM-DD` or RFC 3339 timestamps with explicit `Z` or numeric offset. Date-only values mean midnight UTC. Rendered date cells use the UTC calendar date. An invalid typed date property remains its original scalar; `date(invalid)` returns the empty evaluator value.

`today()` uses the local calendar date from the query's immutable execution environment and encodes it as a date-only value. The worker actor captures the system time and time zone once per request; tests may supply them explicitly. Existing refresh and re-query paths update the environment, but open previews are not scheduled to refresh automatically at midnight.

Natural ordering is locale-independent. ASCII digit runs compare by magnitude without integer conversion, then leading-zero count and bytes. Non-digit text compares case-sensitively by Unicode scalar value. Numeric values compare numerically in relational expressions and when every value in a view sort column or sorted list is numeric. Mixed sort values retain natural ordering; dates and durations retain their type-specific comparisons. Path ties and link fallback selection use natural ordering. Normalized path bytes provide the final path order.

## Results And Limits

View and global filters run before sort and view limits. `matched_count` is pre-limit; `view_count` is post-limit. Result IDs are `r<generation>-<process sequence>` and are never reused during a process. Fetch returns complete rows, not only preview rows.

The result-byte limit is measured against compact JSON for both the query response and complete `{result_id,rows}` payload before rows are cached. The generation-scoped cache admits at most 128 results and 16 MiB of complete serialized row payloads in aggregate, including empty results. These fixed bounds are independent of configured per-result limits. Capacity exhaustion returns `result_too_large` and leaves existing IDs fetchable, including repeated fetches. Successful index publication or initialization releases all cached rows and resets both bounds; failed scans and queries preserve them. Text cells strip C0/C1 controls except tab, LF, and CR. Serialized-byte accounting bounds retained payloads, not allocator overhead or transient query/fetch allocations. The native worker deliberately has no process-wide memory cap; source, expression, AST, YAML, formula, evaluation, row, and result-byte limits remain enforced.
