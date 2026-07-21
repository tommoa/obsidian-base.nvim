# Deterministic fixture vault

`vault/` is a small, self-contained fixture vault. `manifest.json` supplies deterministic file timestamps and expected query results, so tests never rely on host filesystem ctime.

The fixture intentionally covers the Base features used by the current Personal vault:

- Global and view-local filters.
- `this` host context.
- Personal and Work table views.
- Date and duration comparisons.
- Formula dependencies.
- Link conversion and backlinks.
- `#View` selection.
- Missing date properties.
- Unsaved buffer overlays.

It is not an evaluator fixture for historic Base syntax. Those forms are unsupported-feature cases.
