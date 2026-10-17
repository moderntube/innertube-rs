# Vendored SABR protobuf schemas

These `.proto` files are vendored from **[LuanRT/googlevideo](https://github.com/LuanRT/googlevideo)**
(MIT, see `LICENSE-googlevideo`), pinned to the commit in `.googlevideo-commit`.

They define YouTube's SABR wire messages — `VideoPlaybackAbrRequest`, `ClientAbrState`,
`StreamerContext`, the UMP part payloads (`MediaHeader`, `LiveMetadata`,
`StreamProtectionStatus`, `SabrRedirect`, …), and their dependencies.

`build.rs` compiles them with `protox` + `prost-build` into Rust types under
`innertube_sabr::proto`. We depend on the *schema*, not on googlevideo's runtime code —
the generated Rust is our own. To update: re-fetch at a newer commit and bump
`.googlevideo-commit`.

The **UMP container framing** (part `[type][size][payload]`) is not protobuf and is
hand-implemented in `src/lib.rs`; only the part *payloads* and the request are protobuf.
