# Worker Protocol

The worker reads and writes UTF-8 JSON Lines. Each line is one JSON object.
Stdout contains protocol envelopes only; diagnostics go to stderr.

The protocol is closed. Envelope, request, parameter, event, and response
objects reject unknown fields. Numeric IDs must be non-negative integers that
fit exactly in a Lua number. Invalid input receives response ID `0`.

## Envelopes

Requests use:

```json
{"id":1,"request":{"method":"inspect","params":{}}}
```

Successful responses use:

```json
{"id":1,"response":{"type":"success","result":{"method":"inspect","data":{}}}}
```

Failures use:

```json
{"id":1,"response":{"type":"error","error":{"code":"invalid_request","message":"..."}}}
```

Query failures that occur after the Base views are parsed also include
`available_views`, allowing clients to recover from an invalid selected view
without parsing YAML themselves.

Index changes use:

```json
{"event":{"type":"index_changed","generation":2,"paths":["Draft.md"],"origin":"overlay"}}
```

Every request receives one response, including `shutdown`; the worker exits
after emitting the shutdown acknowledgement. Successful overlay mutations emit
`index_changed` with `origin: "overlay"` before their matching response.
Watcher rescans emit the same event with `origin: "watch"`.

## Methods

- `initialize`: `{vault_root, metadata_overrides?, limits?}`. Result:
  `{generation,files}`.
- `query`: `{source,host_path,view_name?,preview_rows?}`. `source` is either
  `{kind:"inline",text,source_id?}` or `{kind:"file",path,source_id?}`.
  Result is the typed table preview, including `result_id`, selected `view`,
  selectable named table `available_views`, columns, preview rows, counts,
  warnings, timings, and `index_generation`.
- `fetch_rows`: `{result_id}`. Result: `{result_id,rows}`.
- `overlay_upsert`: `{path,contents}`. Result: `{generation}`.
- `overlay_commit` and `overlay_remove`: `{path}`. Result: `{generation}`.
- `inspect`: `{}`. Result contains generation, indexed files, overlays, skipped
  path diagnostics, and watcher errors.
- `shutdown`: `{}`. Result: `{}` before process exit.

`limits` accepts only `source_bytes`, `expression_bytes`, `query_ms`,
`evaluation_steps`, `result_rows`, and `result_bytes`; each supplied value must
be a positive integer. Omitted limit fields use the worker defaults.
`preview_rows` is an unsigned integer and defaults to 50.

## Failures

Malformed JSON, invalid request shapes, unknown fields, and out-of-range IDs are
`invalid_request`. Service and evaluator failures use
the existing stable worker codes, including `invalid_path`, `unknown_view`,
`evaluation_limit`, `result_too_large`, and `not_initialized`.
