# Worker Protocol

The worker reads and writes UTF-8 JSON Lines. Each line is one JSON object.
Stdout contains protocol envelopes only; diagnostics go to stderr.
Request lines may contain up to 8 MiB of bytes, excluding the line ending (`\n`
or `\r\n`). Longer lines receive `invalid_request`; the worker discards the
rest of that line and continues reading subsequent requests.

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
Watcher updates emit the same event with `origin: "watch"`. Their `paths`
contain the coalesced file hints, or `[""]` when the watcher cannot retain
a complete named change set. The index may also perform a full scan for
named hints, such as a property-type change or symlink path. Every
publication still invalidates all cached result IDs; these paths describe
change hints, not the scan strategy or query dependencies.
Responses and events share a bounded output queue. When stdout stops draining,
request processing and watcher updates wait for output capacity; clients must
continue reading stdout while sending requests and waiting for shutdown.

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

Successful queries retain complete rows until the next successful index
publication or initialization. Repeated `fetch_rows` calls do not consume a
result. The cache admits at most 128 results and 16 MiB in aggregate compact
JSON `{result_id,rows}` payloads, independently of the per-result limits.
Queries exceeding either cache bound fail with `result_too_large` without
evicting existing IDs. Capacity becomes available after index publication or
initialization; failed scans and failed queries preserve cached rows.

## Failures

Malformed JSON, invalid request shapes, unknown fields, and out-of-range IDs are
`invalid_request`. Service and evaluator failures use
the existing stable worker codes, including `invalid_path`, `unknown_view`,
`evaluation_limit`, `result_too_large`, and `not_initialized`.
