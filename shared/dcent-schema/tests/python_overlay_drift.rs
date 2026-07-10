//! Python `:3000` MCP overlay drift guard — the MCP instance of the
//! author-once / emit-twice / VALIDATE durability mechanism (token-contract §0,
//! `UIVIS-RENDER-1`).
//!
//! WHY THIS EXISTS
//! ---------------
//! `dcent_schema::mcp::minimal_profile()` (this crate, `src/mcp.rs`) is the
//! cross-firmware canonical MCP registry — the upstream spec. Two downstream
//! surfaces re-state that vocabulary by hand:
//!
//!   * the OS REST `/mcp` endpoint binds DIRECTLY to `minimal_profile(...)`
//!     (`dcentrald-api/src/rest.rs`) — NO drift possible, not guarded here;
//!   * the OS Python `:3000` control server
//!     (`DCENT_OS_Antminer/br2_external_dcentos/board/{zynq,amlogic}/rootfs-overlay/
//!     root/web/mcp_server.py`) carries a HAND-MIRRORED `minimal_profile()` dict
//!     plus a `WRITE_TOOLS` set. Those Python literals are duplicated from the
//!     Rust registry by hand and can silently re-drift (the MCP analog of the
//!     stale `theme.ts` mirror the token contract calls out).
//!
//! This test is the "drift check" step of UIVIS-RENDER-1 (token-contract §0
//! step 3): "does each emission match the contract." It drives every assertion
//! FROM the Rust `minimal_profile()` registry, so adding/renaming a tool,
//! editing an alias, or flipping a `write` flag in `src/mcp.rs` turns BOTH
//! Python overlays RED until they are re-aligned. The Python dicts stay readable
//! inline dicts (no behavior change, `py_compile` + the 6-case auth smoke stay
//! byte-green) while becoming a VALIDATED artifact instead of a free-drifting
//! one.
//!
//! It mirrors the proven axe pattern (`dcentaxe-core`
//! `mcp_auth_contract_guards`): `include_str!` the source TEXT, whitespace-
//! collapse it, and assert on the emission FORM (`"name": "<canonical>"`),
//! never byte-exact blocks — so a benign Python reformat cannot flip a guard.
//!
//! Both overlays are pinned: they share the vocab block but are distinct files
//! (amlogic adds a `get_platform_key()` platform shim), so guarding only one
//! would let the other drift.

use dcent_schema::mcp::{minimal_profile, tuning_profile};

/// The zynq (am2 / S9 / S17 / S19) rootfs-overlay control server.
const ZYNQ_MCP_SERVER_PY: &str = include_str!(
    "../../dcentos/br2_external_dcentos/board/zynq/rootfs-overlay/root/web/mcp_server.py"
);

/// The amlogic (S19j Pro AML / S21 / S19k) rootfs-overlay control server.
const AMLOGIC_MCP_SERVER_PY: &str = include_str!(
    "../../dcentos/br2_external_dcentos/board/amlogic/rootfs-overlay/root/web/mcp_server.py"
);

/// Whitespace-collapse a source file so substring assertions are robust to
/// benign reformatting (extra indentation, blank lines, wrapped literals).
/// Identical to the `collapse_ws` helper the axe contract guards use.
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Every overlay we cross-check, paired with a human label for assertion
/// messages.
fn overlays() -> [(&'static str, String); 2] {
    [
        ("zynq", collapse_ws(ZYNQ_MCP_SERVER_PY)),
        ("amlogic", collapse_ws(AMLOGIC_MCP_SERVER_PY)),
    ]
}

/// Both overlay files actually load (guards against an `include_str!` path
/// regression masquerading as a vocab pass).
#[test]
fn both_overlays_are_included_and_nonempty() {
    assert!(
        ZYNQ_MCP_SERVER_PY.len() > 1024,
        "zynq mcp_server.py failed to include (path regression?)"
    );
    assert!(
        AMLOGIC_MCP_SERVER_PY.len() > 1024,
        "amlogic mcp_server.py failed to include (path regression?)"
    );
    // Sanity: each is the MCP control server, not some other file.
    for (label, flat) in overlays() {
        assert!(
            flat.contains("def minimal_profile():"),
            "{label} overlay must define the hand-mirrored minimal_profile() dict"
        );
        assert!(
            flat.contains("WRITE_TOOLS ="),
            "{label} overlay must define the WRITE_TOOLS auth set"
        );
    }
}

/// CORE DRIFT GUARD — driven entirely from the Rust registry.
///
/// For EVERY tool in `minimal_profile("streamable-http")` (the transport the
/// Python `:3000` server emits), assert each Python overlay emits the canonical
/// `"name": "<tool>"` descriptor, carries every `legacyAliases` entry, and
/// classes the tool's write-flag correctly against its `WRITE_TOOLS` set.
///
/// Adding/renaming/removing a tool, editing an alias, or flipping a `write`
/// flag in `src/mcp.rs` makes this fail until both overlays are updated.
#[test]
fn python_overlays_match_the_rust_registry() {
    let profile = minimal_profile("streamable-http");

    // Defense in depth: the registry itself must still be the 6-tool kernel
    // (pinned harder in src/mcp.rs). If that ever changes, the operator is
    // expected to update the overlays in the same change.
    assert_eq!(
        profile.tools.len(),
        6,
        "minimal_profile() kernel size changed — update both Python overlays + this guard"
    );

    for (label, flat) in overlays() {
        for tool in &profile.tools {
            // (1) the canonical EMISSION FORM must be present: `"name": "<tool>"`.
            //     Asserting the descriptor key form (not a bare name substring)
            //     prevents a false PASS from a name that only appears in a
            //     comment/description — same discipline as the axe
            //     `legacy_aliases_accepted_inbound_not_emitted` guard.
            let emission = format!("\"name\": \"{}\"", tool.name);
            assert!(
                flat.contains(&emission),
                "{label} overlay must EMIT canonical MCP tool `{}` as {emission} \
                 (drift from dcent-schema::mcp::minimal_profile)",
                tool.name
            );

            // (2) every legacy alias the registry declares must appear as a
            //     `legacyAliases` string literal in the overlay (accepted
            //     inbound, mapped to the canonical handler).
            for alias in &tool.legacy_aliases {
                let alias_lit = format!("\"{alias}\"");
                assert!(
                    flat.contains(&alias_lit),
                    "{label} overlay must keep legacy alias {alias_lit} for tool `{}` \
                     (drift from dcent-schema::mcp::minimal_profile legacy_aliases)",
                    tool.name
                );
            }

            // (3) write-class must agree with the overlay's WRITE_TOOLS set.
            //     WRITE tools MUST be members; READ tools MUST NOT be members.
            //     The WRITE_TOOLS set is a superset (it also gates the OS-only
            //     hardware tools like write_fpga_register), so for a READ tool
            //     we only assert it is ABSENT, which is the security-relevant
            //     direction (a read tool leaking into WRITE_TOOLS would force a
            //     bearer token on a read path on release images, or — worse, the
            //     inverse — a write tool dropping out would make it callable
            //     unauthenticated).
            let in_write_set = write_tools_set(&flat).iter().any(|m| m == &tool.name);
            if tool.write {
                assert!(
                    in_write_set,
                    "{label} overlay WRITE_TOOLS must contain write tool `{}` \
                     (else it is callable without a bearer token on a release image)",
                    tool.name
                );
            } else {
                assert!(
                    !in_write_set,
                    "{label} overlay WRITE_TOOLS must NOT contain read tool `{}` \
                     (else a read path wrongly requires owner auth on a release image)",
                    tool.name
                );
            }
        }
    }
}

/// Parse the string literals inside the overlay's `WRITE_TOOLS = { ... }` block.
///
/// Whitespace-robust: operates on the collapsed text, slices from the
/// `WRITE_TOOLS =` marker to the next `}`, and extracts the double-quoted
/// entries. Returns an owned `Vec<String>` of member names.
fn write_tools_set(flat: &str) -> Vec<String> {
    let start = flat
        .find("WRITE_TOOLS =")
        .expect("overlay must define WRITE_TOOLS");
    let rest = &flat[start..];
    let open = rest.find('{').expect("WRITE_TOOLS must be a set literal");
    let close = rest[open..]
        .find('}')
        .expect("WRITE_TOOLS set literal must close")
        + open;
    let body = &rest[open + 1..close];

    let mut members = Vec::new();
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            if let Some(rel_end) = body[i + 1..].find('"') {
                members.push(body[i + 1..i + 1 + rel_end].to_string());
                i = i + 1 + rel_end + 1;
                continue;
            }
        }
        i += 1;
    }
    members
}

/// Sanity-pin the WRITE_TOOLS parser against the live overlays: the 3 canonical
/// write tools from the registry must all be present, and none of the 3 read
/// tools may be. Belt-and-suspenders for `python_overlays_match_the_rust_registry`
/// (proves the parser itself isn't silently returning an empty/garbage set).
#[test]
fn write_tools_parser_recovers_the_canonical_write_kernel() {
    let profile = minimal_profile("streamable-http");
    let canonical_writes: Vec<&str> = profile
        .tools
        .iter()
        .filter(|t| t.write)
        .map(|t| t.name.as_str())
        .collect();
    let canonical_reads: Vec<&str> = profile
        .tools
        .iter()
        .filter(|t| !t.write)
        .map(|t| t.name.as_str())
        .collect();

    for (label, flat) in overlays() {
        let set = write_tools_set(&flat);
        assert!(
            set.len() >= 3,
            "{label} WRITE_TOOLS parse looks empty/garbage (got {set:?})"
        );
        for w in &canonical_writes {
            assert!(
                set.iter().any(|m| m == w),
                "{label} WRITE_TOOLS must contain canonical write tool `{w}`"
            );
        }
        for r in &canonical_reads {
            assert!(
                !set.iter().any(|m| m == r),
                "{label} WRITE_TOOLS must NOT contain canonical read tool `{r}`"
            );
        }
    }
}

/// The FULL pinned `WRITE_TOOLS` superset both Python `:3000` overlays must
/// carry, verbatim — the authoritative list of every mutating MCP tool key.
///
/// WHY THIS EXISTS (MCP-1, P1 security, missing-test):
/// `dcent-schema::mcp` only carries the cross-firmware vocabulary (the kernel-6
/// + tuning superset). The 7 **OS-private raw-hardware write tools** below
/// (`write_devmem` / `write_fpga_register` / `write_i2c_register` / `gpio_write`
/// / `board_control` / `service_control` / `set_config`) are NOT in the Rust
/// registry — they live ONLY as live handlers in the Python overlay's `TOOLS`
/// dict and are gated SOLELY by membership in its `WRITE_TOOLS` set. The two
/// registry-driven drift guards above therefore CANNOT see them: a future
/// drop/rename of, say, `write_devmem` out of `WRITE_TOOLS` (while it stays a
/// live `TOOLS` handler) would leave it callable with NO bearer token under the
/// documented `--bind 0.0.0.0` release posture, and stay CI-green.
///
/// This explicit pin closes that gap: it asserts BOTH overlays' `WRITE_TOOLS`
/// set is EXACTLY this superset. It FAILS if any member is dropped/renamed
/// (a write tool would leak unauthenticated on a release image) OR if a new key
/// appears that is not pinned here (a new mutating tool that nobody adjudicated
/// against the auth contract). Updating either overlay's WRITE_TOOLS REQUIRES
/// updating this list in the same change — the intended forcing function.
///
/// Sorted for stable assertion diffs; the source overlays may list in any order.
const PINNED_WRITE_TOOLS_SUPERSET: [&str; 12] = [
    // --- cross-firmware vocabulary (also in dcent-schema::mcp, guarded above) ---
    "identify_device",
    "restart_mining",
    "set_pool",
    "pool_switch",
    "set_fan_speed",
    // --- 7 OS-private raw-hardware / config write tools (NOT in the Rust
    //     registry — pinned ONLY here) ---
    "set_config",
    "write_fpga_register",
    "write_devmem",
    "write_i2c_register",
    "gpio_write",
    "board_control",
    "service_control",
];

/// Parse the string literals inside a named Python set/dict block
/// `<MARKER> ... { ... }` — same whitespace-robust technique as
/// `write_tools_set`, generalized so it can also recover `READ_TOOLS`.
fn brace_block_string_literals(flat: &str, marker: &str) -> Vec<String> {
    let start = flat
        .find(marker)
        .unwrap_or_else(|| panic!("overlay must define {marker}"));
    let rest = &flat[start..];
    let open = rest
        .find('{')
        .unwrap_or_else(|| panic!("{marker} must be a brace block"));
    let close = rest[open..]
        .find('}')
        .unwrap_or_else(|| panic!("{marker} brace block must close"))
        + open;
    let body = &rest[open + 1..close];

    let mut members = Vec::new();
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            if let Some(rel_end) = body[i + 1..].find('"') {
                members.push(body[i + 1..i + 1 + rel_end].to_string());
                i = i + 1 + rel_end + 1;
                continue;
            }
        }
        i += 1;
    }
    members
}

/// Recover the TOOLS dict's top-level handler keys. The overlay declares each
/// tool as `"<name>": {` either inside the literal `TOOLS = { ... }` dict or via
/// a `TOOLS["<name>"] = { ... }` assignment (the dev-only  block). We
/// collect both forms from the collapsed source so the all-classified invariant
/// can be checked against the FULL exposed tool surface.
fn tools_keys(flat: &str) -> std::collections::BTreeSet<String> {
    let mut keys = std::collections::BTreeSet::new();

    // (a) `"<name>": {` literals that appear after the `TOOLS = {` marker
    //     (handler dict entries). We scope from `TOOLS = {` to the trailing
    //     `RESOURCES` marker so unrelated `"k": {` dicts elsewhere aren't picked
    //     up. Both overlays place RESOURCES right after the TOOLS dict.
    let tools_start = flat.find("TOOLS = {").expect("overlay must define TOOLS");
    let scope_end = flat[tools_start..]
        .find("RESOURCES")
        .map(|rel| tools_start + rel)
        .unwrap_or(flat.len());
    let scope = &flat[tools_start..scope_end];
    let bytes = scope.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            if let Some(rel_end) = scope[i + 1..].find('"') {
                let name = &scope[i + 1..i + 1 + rel_end];
                let after = scope[i + 1 + rel_end + 1..].trim_start();
                // Top-level TOOLS entries are `"<name>": { "description": ...`.
                // Filtering on the `"description"` first-key distinguishes a real
                // tool entry from nested `"inputSchema": {` / `"properties": {`
                // blocks (which open with `"type"`/a property name, never
                // `"description"`).
                let opens_tool_entry = after
                    .strip_prefix(": {")
                    .map(|rest| rest.trim_start().starts_with("\"description\""))
                    .unwrap_or(false);
                if opens_tool_entry && is_tool_ident(name) {
                    keys.insert(name.to_string());
                }
                i = i + 1 + rel_end + 1;
                continue;
            }
        }
        i += 1;
    }

    // (b) dev-only `TOOLS["<name>"] = {` assignments (registered when not
    //     _release_image()). These are read-only diagnostic tools and must be
    //     classified too so the invariant holds on dev images.
    let mut search = flat;
    while let Some(rel) = search.find("TOOLS[\"") {
        let from = rel + "TOOLS[\"".len();
        if let Some(end) = search[from..].find('"') {
            let name = &search[from..from + end];
            if is_tool_ident(name) {
                keys.insert(name.to_string());
            }
            search = &search[from + end..];
        } else {
            break;
        }
    }

    keys
}

/// A tool identifier is lowercase snake/alnum — filters out descriptive strings
/// that happen to be followed by `: {` inside nested inputSchema blocks.
fn is_tool_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// MCP-1 DEFENSE-IN-DEPTH GATE PIN — every TOOLS key is classified, and the
/// release auth gate keys off the READ_TOOLS allowlist (fail-closed for unknowns).
///
/// Pins the hardening added to both overlays:
///   1. a `READ_TOOLS` set and a `_tool_requires_auth(` helper both exist;
///   2. the `tools/call` gate calls `_tool_requires_auth(` (NOT the old bare
///      `if tool_name in WRITE_TOOLS:`), so a mutating tool dropped out of
///      WRITE_TOOLS fails CLOSED on a release image instead of falling open;
///   3. READ_TOOLS and WRITE_TOOLS are DISJOINT (a tool can't be both);
///   4. ALL-CLASSIFIED: every live TOOLS handler key is in READ_TOOLS ∪
///      WRITE_TOOLS — so a new mutating tool added to TOOLS but to neither set
///      is denied on release until it is explicitly classified.
#[test]
fn release_auth_gate_is_read_allowlist_fail_closed() {
    use std::collections::BTreeSet;

    for (label, flat) in overlays() {
        // (1) the new primitives exist.
        assert!(
            flat.contains("READ_TOOLS = {"),
            "{label} overlay must define the READ_TOOLS allowlist (MCP-1 fail-closed gate)"
        );
        assert!(
            flat.contains("def _tool_requires_auth("),
            "{label} overlay must define _tool_requires_auth() (MCP-1 read-allowlist classifier)"
        );

        // (2) the gate routes through the read-allowlist classifier, and the old
        //     fail-open `if tool_name in WRITE_TOOLS:` dispatch gate is gone.
        assert!(
            flat.contains("if _tool_requires_auth(tool_name):"),
            "{label} overlay tools/call gate must call _tool_requires_auth(tool_name) \
             so unclassified/dropped mutating tools fail CLOSED on a release image"
        );
        assert!(
            !flat.contains("if tool_name in WRITE_TOOLS:"),
            "{label} overlay still has the fail-OPEN `if tool_name in WRITE_TOOLS:` gate — \
             a write tool dropped from WRITE_TOOLS would skip auth on a release image"
        );

        let read_set: BTreeSet<String> = brace_block_string_literals(&flat, "READ_TOOLS = {")
            .into_iter()
            .collect();
        let write_set: BTreeSet<String> = write_tools_set(&flat).into_iter().collect();

        // (3) disjoint.
        let both: Vec<&String> = read_set.intersection(&write_set).collect();
        assert!(
            both.is_empty(),
            "{label} overlay tools {both:?} are in BOTH READ_TOOLS and WRITE_TOOLS \
             (a tool must be classified read XOR write)"
        );

        // (4) all-classified — every exposed TOOLS handler key is read or write.
        let classified: BTreeSet<String> = read_set.union(&write_set).cloned().collect();
        let unclassified: Vec<String> = tools_keys(&flat)
            .into_iter()
            .filter(|k| !classified.contains(k))
            .collect();
        assert!(
            unclassified.is_empty(),
            "{label} overlay exposes TOOLS handler(s) {unclassified:?} that are in NEITHER \
             READ_TOOLS nor WRITE_TOOLS — an unclassified mutating tool would be denied on \
             release (fail-closed) but this MUST be made explicit: add each to READ_TOOLS \
             (read-only) or WRITE_TOOLS (mutating + PINNED_WRITE_TOOLS_SUPERSET)."
        );
    }
}

/// MCP-1 CORE PIN — exact WRITE_TOOLS superset on BOTH overlays.
///
/// The security-relevant property: every mutating tool the `:3000` server can
/// dispatch MUST be a `WRITE_TOOLS` member, because that membership is the ONLY
/// thing that routes it through `_write_tool_authorized()` (open-on-dev /
/// bearer-required-and-fail-closed-on-release). A mutating tool that is NOT in
/// `WRITE_TOOLS` skips the gate entirely and is callable unauthenticated on a
/// release image under `--bind 0.0.0.0`.
///
/// Asserts SET EQUALITY (not just superset/subset) against
/// `PINNED_WRITE_TOOLS_SUPERSET` for each overlay, so the test fails on BOTH
/// directions of drift:
///   * a pinned write tool dropped/renamed OUT of WRITE_TOOLS  → unauth write leak;
///   * a new key added INTO WRITE_TOOLS that isn't pinned here → unreviewed
///     mutating tool (forces an explicit adjudication in this test).
#[test]
fn pinned_write_tools_superset_matches_both_overlays_exactly() {
    use std::collections::BTreeSet;

    let pinned: BTreeSet<String> = PINNED_WRITE_TOOLS_SUPERSET
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(
        pinned.len(),
        PINNED_WRITE_TOOLS_SUPERSET.len(),
        "PINNED_WRITE_TOOLS_SUPERSET has a duplicate entry"
    );

    for (label, flat) in overlays() {
        let actual: BTreeSet<String> = write_tools_set(&flat).into_iter().collect();

        // Dropped/renamed out of WRITE_TOOLS → callable without a bearer token
        // on a release image. This is the load-bearing direction.
        let missing: Vec<&String> = pinned.difference(&actual).collect();
        assert!(
            missing.is_empty(),
            "{label} overlay WRITE_TOOLS is MISSING pinned mutating tool(s) {missing:?} \
             — each would be callable WITHOUT a bearer token on a release image \
             (raw-hardware write exposed under --bind 0.0.0.0). Re-add them to \
             WRITE_TOOLS, or if a tool was intentionally removed, drop it from its \
             TOOLS handler AND from PINNED_WRITE_TOOLS_SUPERSET in the same change."
        );

        // A new mutating key appeared that nobody adjudicated against this pin.
        let unexpected: Vec<&String> = actual.difference(&pinned).collect();
        assert!(
            unexpected.is_empty(),
            "{label} overlay WRITE_TOOLS contains UNPINNED key(s) {unexpected:?} \
             — a new mutating MCP tool must be added to PINNED_WRITE_TOOLS_SUPERSET \
             here (and confirmed it routes through _write_tool_authorized) before it ships."
        );

        assert_eq!(
            actual, pinned,
            "{label} overlay WRITE_TOOLS drifted from the pinned superset"
        );
    }
}

/// MCP-1 DEFENSE-IN-DEPTH PIN — every pinned mutating tool is also a live
/// `TOOLS` handler in both overlays (else WRITE_TOOLS would be gating a phantom),
/// and the 7 OS-private raw-hardware write tools are present as handlers.
///
/// The registry drift guards above already prove the cross-firmware 5 are
/// exposed; this nails down the 7 OS-private ones (which the Rust registry never
/// sees) as live `TOOLS` keys, so a silent handler rename can't pass merely by
/// also editing WRITE_TOOLS — the handler key and the gate set must move together.
#[test]
fn os_private_raw_hardware_write_tools_are_live_handlers_in_both_overlays() {
    const OS_PRIVATE_RAW_WRITE_TOOLS: [&str; 7] = [
        "set_config",
        "write_fpga_register",
        "write_devmem",
        "write_i2c_register",
        "gpio_write",
        "board_control",
        "service_control",
    ];

    for (label, flat) in overlays() {
        // (a) every pinned write tool is a live TOOLS handler key `"<tool>": {`.
        for tool in PINNED_WRITE_TOOLS_SUPERSET {
            assert!(
                flat.contains(&format!("\"{tool}\": {{")),
                "{label} overlay must define a live TOOLS handler `\"{tool}\": {{ ... }}` \
                 for pinned WRITE_TOOLS member `{tool}` (WRITE_TOOLS must not gate a phantom)"
            );
        }

        // (b) the 7 OS-private raw-hardware write tools (invisible to the Rust
        //     registry) must each be present AND in WRITE_TOOLS.
        let write_set = write_tools_set(&flat);
        for tool in OS_PRIVATE_RAW_WRITE_TOOLS {
            assert!(
                flat.contains(&format!("\"{tool}\": {{")),
                "{label} overlay must keep OS-private raw-hardware write tool `{tool}` \
                 as a live TOOLS handler"
            );
            assert!(
                write_set.iter().any(|m| m == tool),
                "{label} overlay WRITE_TOOLS must contain OS-private raw-hardware write \
                 tool `{tool}` (else it is callable without a bearer token on a release image)"
            );
        }
    }
}

/// True if the overlay EXPOSES `name` as a canonical MCP tool, in either of the
/// two forms the Python `:3000` server uses in SOURCE TEXT:
///   * a `minimal_profile()` descriptor emission `"name": "<tool>"` (the kernel
///     6 live in the hand-mirrored `minimal_profile()` dict this way), OR
///   * a `TOOLS` dict KEY `"<tool>": {` (the richer tools — e.g. `set_fan_speed`
///     — live in the `TOOLS` registry, and `tools/list` emits their name from
///     the dict key via `{"name": name, ...}`, so the literal `"name":
///     "set_fan_speed"` never appears in source).
/// Asserting on these two descriptor FORMS (never a bare substring) keeps a name
/// that only appears in a comment/description from producing a false PASS — the
/// same discipline as the kernel drift guard above.
fn overlay_exposes(flat: &str, name: &str) -> bool {
    flat.contains(&format!("\"name\": \"{name}\"")) || flat.contains(&format!("\"{name}\": {{"))
}

/// S5 SUPERSET DRIFT GUARD — driven entirely from `tuning_profile()`.
///
/// `tuning_profile()` (dcent-schema) is the cross-firmware tuning superset: the
/// locked kernel 6 plus 6 richer tools. The OS `:3000` overlay intentionally
/// mirrors only a SUBSET of the superset — the kernel 6 plus `set_fan_speed` —
/// because the other 5 extensions (`set_frequency` / `set_core_voltage` /
/// `run_autotune` / `get_network` / `get_history`) are axe-only by design
/// (keep-unique fence, mcp-auth-contract §2.2/§6). OS reaches freq/voltage
/// EFFECTS through its OWN low-level tools (`set_config` / `write_i2c_register`),
/// which are NOT superset aliases and stay OS-private.
///
/// EXPOSURE POLICY LIVES IN THIS TEST, never in the schema registry — so the
/// superset registry can carry the full 12-tool vocabulary without forcing the
/// OS surface to grow the 5 axe-only tools.
///
/// Two guarantees, both driven FROM `tuning_profile()`:
///   1. HARD requirement — every OS-exposed superset tool (the allow-list below)
///      must be emitted canonically + carry every legacy alias the registry
///      declares.
///   2. FAIL-CLOSED safety net for ALL 12 — if (and only if) the overlay exposes
///      a tool, its write-class MUST agree with WRITE_TOOLS membership. A CONTROL
///      tool exposed WITHOUT WRITE_TOOLS membership would be callable with no
///      bearer token on a release image (AOTA-class control-without-auth); a READ
///      tool wrongly in WRITE_TOOLS would force owner auth on a read path. Tools
///      the overlay does NOT expose carry no constraint — which is exactly what
///      lets OS keep the 5 axe-only tools off its surface.
#[test]
fn python_overlays_match_the_tuning_superset() {
    let profile = tuning_profile("streamable-http");

    // Defense in depth: the superset must still be the 12-tool shape (pinned
    // harder in src/mcp.rs). If it changes, update this guard + the exposure
    // policy in the same change.
    assert_eq!(
        profile.tools.len(),
        12,
        "tuning_profile() superset size changed — update both overlays + this guard + OS_3000_EXPOSED"
    );

    // The OS `:3000` exposure allow-list: kernel 6 + set_fan_speed. The 5
    // remaining superset tools are axe-only and intentionally absent on OS.
    const OS_3000_EXPOSED: [&str; 7] = [
        "get_status",
        "get_device_info",
        "get_swarm_status",
        "identify_device",
        "restart_mining",
        "set_pool",
        "set_fan_speed",
    ];

    for (label, flat) in overlays() {
        let write_set = write_tools_set(&flat);

        for tool in &profile.tools {
            let exposed = overlay_exposes(&flat, &tool.name);

            // (1) HARD requirement for the OS-exposed subset.
            if OS_3000_EXPOSED.contains(&tool.name.as_str()) {
                assert!(
                    exposed,
                    "{label} overlay must EXPOSE OS superset tool `{}` (kernel-6 + set_fan_speed) \
                     as a canonical descriptor (drift from dcent-schema::mcp::tuning_profile)",
                    tool.name
                );
                for alias in &tool.legacy_aliases {
                    let alias_lit = format!("\"{alias}\"");
                    assert!(
                        flat.contains(&alias_lit),
                        "{label} overlay must keep legacy alias {alias_lit} for OS superset tool `{}` \
                         (drift from tuning_profile legacy_aliases)",
                        tool.name
                    );
                }
            }

            // (2) FAIL-CLOSED safety net for ALL 12 — only constrains EXPOSED
            //     tools, so OS is never forced to grow the 5 axe-only tools, yet
            //     a tuning CONTROL tool can never be exposed without WRITE_TOOLS
            //     membership.
            if exposed {
                let in_write_set = write_set.iter().any(|m| m == &tool.name);
                if tool.write {
                    assert!(
                        in_write_set,
                        "{label} overlay exposes CONTROL superset tool `{}` but it is ABSENT from \
                         WRITE_TOOLS (callable without a bearer token on a release image)",
                        tool.name
                    );
                } else {
                    assert!(
                        !in_write_set,
                        "{label} overlay exposes READ superset tool `{}` but it is IN WRITE_TOOLS \
                         (would force owner auth on a read path on a release image)",
                        tool.name
                    );
                }
            }
        }
    }
}

/// WAVE-7 BEHAVIORAL SANITIZER GUARD (privacy, missing-test).
///
/// Every guard ABOVE pins the overlay SOURCE TEXT — none EXECUTES the privacy
/// sanitizer. The load-bearing `sanitize_secrets()` (with `_BECH32_RE` /
/// `_BASE58_RE` / `_HEX_RE` / `_URL_CRED_RE` / `_mask_wallet`) is what stops a
/// READ tool from leaking the operator's BTC payout address — on V1 solo the
/// pool `worker` IS a full bech32/base58 address — or a pool `user:pass@`
/// credential to anyone who can reach `:3000`. A future regex edit could
/// silently stop masking and stay CI-green, because no test ever ran the
/// regexes against real inputs. This closes that gap BEHAVIORALLY.
///
/// For EACH overlay it loads the module BY FILE PATH via importlib (tolerating a
/// `SystemExit` on import — the same approach as the coordinator's runtime smoke
/// test), runs `sanitize_secrets()` over a fixture table (bech32 P2WPKH worker
/// line, bech32m P2TR, base58 P2PKH, 64-hex, a `user:pass@` pool URL, and a
/// clean URL), and asserts NONE of the raw secrets survive while the
/// credential-free URL's host is preserved.
///
/// ROBUSTNESS: if `python3` is not on PATH (`Command` spawn `NotFound`), the
/// test SKIPS (`eprintln!` + `return`) so a python3-less host does not
/// false-fail; when python3 IS present (WSL/CI, python3.10 at `/usr/bin/python3`)
/// the assertions run. `PYTHONIOENCODING=utf-8` is forced so the `…` (U+2026)
/// mask glyph prints regardless of the host locale.
#[test]
fn mcp_sanitizer_behaviorally_masks_wallets_and_credentials() {
    use std::path::PathBuf;
    use std::process::Command;

    // Raw secrets that MUST be masked/stripped by sanitize_secrets(). None of
    // these full strings may survive anywhere in the sanitized output.
    const BECH32_P2WPKH: &str = "bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq";
    const BECH32M_P2TR: &str = "bc1p5d7rjq7g6rdk2yhzks9smlaqtedr4dekq08ge8ztwac72sfr9rusxg3297";
    const BASE58_P2PKH: &str = "1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa";
    const HEX64: &str = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
    const CLEAN_URL: &str = "stratum+tcp://pool.example.com:3333";
    const CLEAN_HOST: &str = "pool.example.com";

    // Python driver: load the overlay module from its file PATH (argv[1]) via
    // importlib, tolerate a SystemExit on import, run sanitize_secrets() over the
    // fixtures, and print one deterministic `OUT|<key>|<masked>` line per
    // fixture. The Rust side owns all assertions (file discipline). The fixture
    // literals here are duplicated as Rust consts above for the substring checks.
    const PY_DRIVER: &str = r#"
import sys, importlib.util
mod_path = sys.argv[1]
spec = importlib.util.spec_from_file_location("mcp_overlay_under_test", mod_path)
mod = importlib.util.module_from_spec(spec)
try:
    spec.loader.exec_module(mod)
except SystemExit:
    pass
sanitize = mod.sanitize_secrets
fixtures = {
    "worker_line": "worker=bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq.rig1",
    "p2tr": "bc1p5d7rjq7g6rdk2yhzks9smlaqtedr4dekq08ge8ztwac72sfr9rusxg3297",
    "p2pkh": "1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa",
    "hex64": "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
    "cred_url": "stratum+tcp://user:secretpass@pool.example.com:3333",
    "clean_url": "stratum+tcp://pool.example.com:3333",
}
for key in sorted(fixtures):
    print("OUT|%s|%s" % (key, sanitize(fixtures[key])))
"#;

    // Probe for a WORKING python3 BEFORE running the driver. On Windows the
    // `python3` "App Execution Alias" ships by default and is NOT a real
    // interpreter: it spawns successfully, prints a Microsoft-Store install
    // hint to stderr, and exits non-zero. The old skip path only caught a spawn
    // `ErrorKind::NotFound`, so that stub made this test HARD-FAIL on a stock
    // Windows dev host — the exact false-fail the doc comment above promises we
    // avoid. Treat a non-zero `python3 --version`, or a banner that carries the
    // Store "was not found" hint instead of a real "Python 3.x" version line,
    // as "python3 absent" and SKIP. When real CPython is present (WSL/CI,
    // python3.11 at /usr/bin/python3) the probe passes and every assertion runs.
    fn python3_usable() -> bool {
        let probe = match std::process::Command::new("python3")
            .arg("--version")
            .output()
        {
            Ok(o) => o,
            Err(_) => return false, // NotFound / spawn failure
        };
        if !probe.status.success() {
            return false; // Windows Store alias stub exits non-zero
        }
        let banner = format!(
            "{}{}",
            String::from_utf8_lossy(&probe.stdout),
            String::from_utf8_lossy(&probe.stderr)
        );
        banner.contains("Python 3") && !banner.contains("was not found")
    }

    if !python3_usable() {
        eprintln!(
            "SKIP mcp_sanitizer_behaviorally_masks_wallets_and_credentials: \
             no working python3 on PATH (a Windows App Execution Alias stub \
             counts as absent); behavioral sanitizer assertions not run on this \
             host. (Runs under WSL/CI where python3.11 is at /usr/bin/python3.)"
        );
        return;
    }

    // Overlay file paths resolved the SAME way the include_str! guards above do,
    // but rooted at CARGO_MANIFEST_DIR (the crate root, one level ABOVE tests/)
    // for a runtime filesystem path: `../dcentos/...` instead of the source-file
    // relative `../../dcentos/...` the include_str! macros use.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let overlays = [
        (
            "zynq",
            manifest_dir.join(
                "../dcentos/br2_external_dcentos/board/zynq/rootfs-overlay/root/web/mcp_server.py",
            ),
        ),
        (
            "amlogic",
            manifest_dir.join(
                "../dcentos/br2_external_dcentos/board/amlogic/rootfs-overlay/root/web/mcp_server.py",
            ),
        ),
    ];

    for (label, overlay_path) in overlays {
        assert!(
            overlay_path.exists(),
            "{label} overlay path does not exist: {} (path regression?)",
            overlay_path.display()
        );

        let output = match Command::new("python3")
            .arg("-c")
            .arg(PY_DRIVER)
            .arg(&overlay_path)
            .env("PYTHONIOENCODING", "utf-8")
            .output()
        {
            Ok(o) => o,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!(
                    "SKIP mcp_sanitizer_behaviorally_masks_wallets_and_credentials: \
                     python3 not found on PATH ({e}); behavioral sanitizer assertions \
                     not run on this host. (Runs under WSL/CI where python3.10 is at \
                     /usr/bin/python3.)"
                );
                return;
            }
            Err(e) => panic!("failed to spawn python3 for {label} overlay: {e}"),
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "{label} overlay sanitizer driver exited non-zero (sanitize_secrets \
             missing / regex broken / import failed).\n--- stdout ---\n{stdout}\n\
             --- stderr ---\n{stderr}"
        );

        // (1) NONE of the full raw secrets may survive anywhere in the masked
        //     output. `secretpass` and `:secretpass@` cover the pool-URL
        //     credential strip (both the bare password and the userinfo form).
        for raw in [
            BECH32_P2WPKH,
            BECH32M_P2TR,
            BASE58_P2PKH,
            HEX64,
            "secretpass",
            ":secretpass@",
        ] {
            assert!(
                !stdout.contains(raw),
                "{label} overlay sanitize_secrets() LEAKED raw secret `{raw}` — \
                 wallet/credential masking regressed.\n--- masked output ---\n{stdout}"
            );
        }

        // (2) POSITIVE check the sanitizer actually MASKED (not just returned the
        //     input empty/unchanged): the bech32 mask form `<first6>…<last4>`
        //     (U+2026 ellipsis) must appear for the worker-line address.
        assert!(
            stdout.contains("bc1qar…5mdq"),
            "{label} overlay sanitize_secrets() did not emit the expected masked \
             bech32 form `bc1qar…5mdq` — the regex matched nothing (a silent \
             no-op is the exact regression this guards).\n--- masked output ---\n{stdout}"
        );

        // (3) The credential-free URL's host MUST be preserved (the sanitizer
        //     must strip userinfo, never over-strip a clean URL).
        let clean_line = stdout
            .lines()
            .find(|l| l.starts_with("OUT|clean_url|"))
            .unwrap_or_else(|| panic!("{label}: missing clean_url output line\n{stdout}"));
        assert!(
            clean_line.contains(CLEAN_URL),
            "{label} overlay sanitize_secrets() altered a credential-free URL \
             (host `{CLEAN_HOST}` must be preserved verbatim): {clean_line}"
        );

        // (4) The credential URL must KEEP its host while dropping the userinfo.
        let cred_line = stdout
            .lines()
            .find(|l| l.starts_with("OUT|cred_url|"))
            .unwrap_or_else(|| panic!("{label}: missing cred_url output line\n{stdout}"));
        assert!(
            cred_line.contains(CLEAN_HOST),
            "{label} overlay sanitize_secrets() dropped the host while stripping \
             credentials (host `{CLEAN_HOST}` must survive): {cred_line}"
        );
    }
}
