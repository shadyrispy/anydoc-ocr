fix(inference): derive the ONNX input name from the session instead of hard-coding `"x"`

## Problem

`OrtInfer::new` and `OrtInfer::from_config` fall back to a literal `"x"` when the
caller passes `input_name: None`:

```rust
input_name: input_name.unwrap_or("x").to_string(),
```

Exported graphs disagree on the input name. PaddleOCR detectors declare `x`, but
others declare something else — `seal_PP-OCRv4_det` declares `image`. Against such a
graph the feed key does not match, and `session.run` fails at inference time with an
ORT error that the CLI never surfaces, so the model looks simply broken.

This is also the reason `input_names_from_model` — added in #93 — could never be used
to work around the problem: construction already had a name baked in, and the
method's result was not consulted.

## Fix

Read the first declared input name off the session that was just built, and only
fall back to `"x"` for a graph that declares no inputs:

```rust
fn first_input_name(session: &Session) -> String {
    session
        .inputs()
        .first()
        .map(|i| i.name().to_string())
        .unwrap_or_else(|| "x".to_string())
}
```

Both constructors now resolve the name before the struct literal (fields evaluate in
declaration order, so `sessions` moves `session` out before `input_name` can read it).

`db.rs` opts into the detection by passing `None` instead of `Some("x")`, which is
what makes the DB detector work on graphs like the seal model.

## Behaviour

- Passing an explicit `input_name` is unchanged, including the `Some("x")` callers.
- Graphs declaring `x` — every PaddleOCR detector in the test matrix — take the same
  path as before, so this is a no-op for them.
- Output is bit-identical on all 32 golden snapshots of a downstream consumer
  (PDF/OFD → GFM pipeline, CPU-only, ONNX Runtime 1.28.2).

## Testing

Compiled and ran the downstream test suite against this patch: 382 unit tests pass,
all 32 golden snapshots byte-identical. The patch applies cleanly to `main`
(`patch -p1`, no fuzz).

Happy to add a regression test that builds a tiny ONNX graph declaring a non-`x`
input if you would prefer that covered in-tree — I did not add one because the
downstream consumer has no fixture model to commit.
