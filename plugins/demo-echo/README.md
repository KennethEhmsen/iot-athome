# demo-echo

Reference plugin used to validate the manifest shape, the plugin-host loader,
and capability enforcement end-to-end.

Status: **manifest only** at W1. The WASM component source + build lands in
M2 alongside the WIT world and `wit-bindgen` wiring. The manifest validates
against `schemas/plugin-manifest.schema.json` today; CI can use it as a
fixture.
